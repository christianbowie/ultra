//! Deterministic line-level boilerplate stripping against near-identical
//! neighbors.
//!
//! **Why not chunk-hash?** The embedding pipeline already filters chunks
//! whose `content_hash` repeats across ≥ `boilerplate_min_atom_count` atoms
//! (see [`crate::embedding`]). That works for documents large enough that a
//! shared section lands in its own chunk. Typical boilerplate-polluted
//! atoms in this KB, however, are short runbook-style notes that fit
//! entirely inside a single chunk — so the whole atom hashes to one value
//! and the filter can't see the shared *sub*-content.
//!
//! **Why not LLM?** [`crate::health::llm_fixes::strip_boilerplate_atom`]
//! asks an LLM to identify and remove template text. It's brittle for
//! runbook notes whose "boilerplate" is *structural* (shared headers, table
//! skeletons, field labels) rather than literal copy-paste. The model
//! treats every value as subject-specific and returns the input unchanged,
//! producing an empty-diff UI that confused users into thinking Strip was
//! broken.
//!
//! **This module's approach:** compare the target atom line-by-line
//! against its near-identical neighbors (edges at similarity ≥ the same
//! threshold the boilerplate-pollution check uses). Any line that appears
//! in at least `boilerplate_min_atoms` distinct neighbors is treated as
//! shared template and removed. The remaining lines are rejoined with the
//! original blank-line spacing. The result is deterministic, never hits
//! the LLM, and is guaranteed to either shrink content or be a no-op.
//!
//! Callers must check `original == proposed` to detect the no-op case.

use crate::error::AtomicCoreError;
use crate::health::audit;
use crate::health::types::FixAction;
use crate::AtomicCore;
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Dry-run flag default for content-mutating fixes.
pub type StripOutcome = (String, Option<FixAction>);

/// Threshold above which a normalized line appearing in that many distinct
/// neighbor atoms is treated as shared boilerplate. Matches the default
/// `boilerplate_min_atom_count` the embedding pipeline uses.
const DEFAULT_MIN_NEIGHBOR_MATCHES: usize = 2;

/// Minimum similarity when gathering neighbors. 0.9 is the floor beneath
/// which neighbor content overlap is usually too noisy to treat as shared
/// template. The boilerplate-pollution check itself surfaces atoms with
/// edges at >= `thresholds.boilerplate_similarity` (default 0.99); the
/// lower floor here additionally catches neighbors that fell just below
/// the strict cutoff but which still share most of their lines with the
/// target.
const DEFAULT_NEIGHBOR_SIMILARITY: f32 = 0.9;

/// Cap on the fallback atom sample when no semantic neighbors are known.
/// Keeps the comparison pass O(1) relative to KB size — ~100 atoms is
/// large enough to detect shared templates in a runbook-style KB without
/// turning every Strip click into a full-table scan.
const FALLBACK_SAMPLE_SIZE: i32 = 100;
/// Lines shorter than this (after trimming) are always kept — they're
/// typically blank lines or single punctuation characters and removing
/// them produces ugly output without meaningful content reduction.
const SHORT_LINE_KEEP_CHARS: usize = 3;

/// Strip lines from `atom_id` whose normalized form appears in ≥
/// `min_neighbor_matches` of the atom's near-identical neighbors.
///
/// Returns `(new_content, action_opt)`. When `dry_run` is `true` or when
/// nothing was shared, `action_opt` is `None` and no mutation happens.
/// Callers **must** treat `new_content == original_content` as "no shared
/// lines found" and surface that clearly instead of showing an empty diff.
pub async fn strip_shared_chunks_atom(
    core: &AtomicCore,
    atom_id: &str,
    dry_run: bool,
) -> Result<StripOutcome, AtomicCoreError> {
    let atom = match core.get_atom(atom_id).await? {
        Some(a) => a,
        None => {
            return Err(AtomicCoreError::NotFound(format!(
                "atom {atom_id} not found"
            )));
        }
    };
    if atom.atom.is_locked {
        return Err(AtomicCoreError::Validation(format!(
            "atom {atom_id} is locked — unlock it before stripping"
        )));
    }

    let original = atom.atom.content.clone();
    if original.trim().is_empty() {
        return Ok((original, None));
    }

    // Pull direct neighbors (depth 1) above the similarity floor. Using the
    // existing neighborhood query keeps this consistent with the health
    // report's definition of "near-identical" and avoids a second query
    // path that could diverge from the check.
    let graph = core
        .get_atom_neighborhood(atom_id, 1, DEFAULT_NEIGHBOR_SIMILARITY)
        .await?;
    let mut neighbor_contents: Vec<String> = graph
        .atoms
        .iter()
        .filter(|n| n.atom.atom.id != atom.atom.id)
        .map(|n| n.atom.atom.content.clone())
        .collect();

    // Fallback: if the atom has no recorded neighbors (edges may be stale
    // after a dimension change, or the pipeline's own boilerplate filter
    // stripped shared chunks so cosine never rose high enough to record an
    // edge) use a bounded sample of recent atoms as the comparison
    // population. Callers who hit Strip from the dashboard have already
    // seen this atom flagged as polluted, so we owe them a best-effort
    // answer even when the edge table can't supply one.
    if neighbor_contents.is_empty() {
        let params = crate::ListAtomsParams {
            tag_id: None,
            limit: FALLBACK_SAMPLE_SIZE,
            offset: 0,
            cursor: None,
            cursor_id: None,
            source_filter: crate::SourceFilter::All,
            source_value: None,
            sort_by: crate::SortField::Updated,
            sort_order: crate::SortOrder::Desc,
        };
        let page = core.list_atoms(&params).await?;
        for summary in page.atoms.into_iter().filter(|a| a.id != atom.atom.id) {
            if let Some(content) = core
                .storage()
                .get_atom_content_impl(&summary.id)
                .await?
            {
                neighbor_contents.push(content);
            }
        }
    }

    if neighbor_contents.is_empty() {
        // No neighbors — nothing to compare against. UI must surface this
        // as a no-op (same path as "no shared lines found").
        return Ok((original, None));
    }

    let settings = core.get_settings_map().await.unwrap_or_default();
    // Dynamic default: when the atom has many near-identical neighbors,
    // requiring a line to match in 2+ of them hides partial clusters where
    // a runbook template has many siblings but each shares only a subset
    // of labels. We scale the floor down to `ceil(len / 4)` for 3+
    // neighbors (min 1), so a 8-sibling cluster like the app-support
    // runbooks strips any line present in >= 2 siblings. An explicit
    // `boilerplate_strip_min_neighbor_matches` setting still wins.
    let dynamic_default: usize = if neighbor_contents.len() >= 3 {
        (neighbor_contents.len() / 4).max(1)
    } else {
        DEFAULT_MIN_NEIGHBOR_MATCHES
    };
    let min_matches = settings
        .get("boilerplate_strip_min_neighbor_matches")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(dynamic_default)
        // Can't require more matches than we have neighbors.
        .min(neighbor_contents.len());

    // Build a per-line multiset across neighbors: normalized_line →
    // count_of_distinct_neighbors_that_contain_it. We count per-neighbor
    // presence (not per-occurrence) so a neighbor that repeats a line
    // doesn't falsely inflate the score.
    let mut line_counts: HashMap<String, usize> = HashMap::new();
    for neighbor in &neighbor_contents {
        let seen: HashSet<String> = neighbor
            .lines()
            .filter_map(normalize_line_for_match)
            .collect();
        for line in seen {
            *line_counts.entry(line).or_insert(0) += 1;
        }
    }

    // Walk the target's lines and drop anything hitting the threshold.
    let mut kept: Vec<&str> = Vec::new();
    let mut stripped = 0usize;
    for raw_line in original.lines() {
        let keep = match normalize_line_for_match(raw_line) {
            // Short/blank/ignorable line — always keep to preserve layout.
            None => true,
            Some(norm) => match line_counts.get(&norm) {
                Some(count) if *count >= min_matches => false,
                _ => true,
            },
        };
        if keep {
            kept.push(raw_line);
        } else {
            stripped += 1;
        }
    }

    if stripped == 0 {
        return Ok((original, None));
    }

    // Rebuild, collapsing runs of blank lines that the strip left behind
    // into a single blank line each so we don't emit a page of vertical
    // whitespace between surviving sections.
    let new_content = compact_blank_runs(&kept);

    if new_content.trim().is_empty() {
        // Every line was shared — refuse to clear the atom (pipeline's
        // boilerplate filter falls back to "embed everything" in this case
        // too, so returning the original is strictly safer than emitting
        // empty content).
        return Ok((original, None));
    }
    if new_content == original {
        return Ok((original, None));
    }

    if dry_run {
        return Ok((new_content, None));
    }

    let before_state = json!({
        "id": atom.atom.id,
        "content": original,
        "source_url": atom.atom.source_url,
    });
    let upd = crate::UpdateAtomRequest {
        content: new_content.clone(),
        source_url: atom.atom.source_url.clone(),
        published_at: atom.atom.published_at.clone(),
        tag_ids: Some(atom.tags.iter().map(|t| t.id.clone()).collect()),
    };
    core.update_atom(&atom.atom.id, upd, |_| {}).await?;

    // Auto-dismiss the boilerplate_pollution entry for this atom. The
    // pipeline will re-embed and recompute semantic edges; if the
    // remaining content still overlaps with siblings, the atom would
    // otherwise flash back into the review queue within seconds. The user
    // stripped what our line-level comparison identified as shared — if
    // the row needs to reappear the user can un-dismiss or re-scan to
    // surface it intentionally.
    if let Err(e) = core
        .dismiss_health_item(
            "boilerplate_pollution",
            &atom.atom.id,
            "Stripped shared lines — dismissed pending re-embed",
            None,
        )
        .await
    {
        tracing::warn!(atom_id = %atom.atom.id, error = %e, "failed to auto-dismiss boilerplate_pollution after strip");
    }

    let fix_id = audit::log_fix(
        core,
        "boilerplate_pollution",
        "strip_shared_chunks",
        "low",
        Some(std::slice::from_ref(&atom.atom.id)),
        None,
        before_state,
        json!({
            "stripped_lines": stripped,
            "neighbor_count": neighbor_contents.len(),
            "min_matches": min_matches,
            "new_length": new_content.len(),
        }),
        None,
        None,
    )
    .await?;

    Ok((
        new_content,
        Some(FixAction {
            id: fix_id,
            check: "boilerplate_pollution".to_string(),
            action: "strip_shared_chunks".to_string(),
            count: 1,
            details: vec![format!(
                "Stripped {} shared line(s) from {}",
                stripped, atom.atom.id,
            )],
        }),
    ))
}

/// Map a raw line to the canonical form used for cross-atom matching.
///
/// Returns `None` for lines that are blank or too short to be meaningful
/// boilerplate signal — those are preserved in the output unconditionally
/// to keep paragraph spacing intact. Otherwise trims whitespace and
/// collapses internal runs of spaces so trivial whitespace differences
/// across atoms don't defeat matching.
fn normalize_line_for_match(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.len() < SHORT_LINE_KEEP_CHARS {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    Some(out)
}

/// Rebuild content from kept lines, collapsing any run of consecutive
/// blank lines left behind by stripped content into a single blank line.
/// Trailing blank lines are removed entirely.
fn compact_blank_runs(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut blank_run = false;
    for (i, line) in lines.iter().enumerate() {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if blank_run || out.is_empty() {
                continue;
            }
            blank_run = true;
            if i + 1 < lines.len() {
                out.push('\n');
            }
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if blank_run {
                out.push('\n');
            }
            out.push_str(line);
            blank_run = false;
        }
    }
    // Trim trailing newlines without stripping interior spacing.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_line_trims_and_collapses_whitespace() {
        assert_eq!(
            normalize_line_for_match("   ## Health endpoints   ").as_deref(),
            Some("## Health endpoints"),
        );
        assert_eq!(
            normalize_line_for_match("foo\tbar\t\tbaz").as_deref(),
            Some("foo bar baz"),
        );
    }

    #[test]
    fn normalize_line_returns_none_for_short_lines() {
        assert_eq!(normalize_line_for_match(""), None);
        assert_eq!(normalize_line_for_match("  "), None);
        assert_eq!(normalize_line_for_match("#"), None);
    }

    #[test]
    fn compact_blank_runs_collapses_multiples() {
        let lines = vec!["alpha", "", "", "beta", "", "", "", "gamma"];
        assert_eq!(compact_blank_runs(&lines), "alpha\n\nbeta\n\ngamma");
    }

    #[test]
    fn compact_blank_runs_trims_trailing_blanks() {
        let lines = vec!["alpha", "", ""];
        assert_eq!(compact_blank_runs(&lines), "alpha");
    }
}
