//! Integration tests for line-level boilerplate stripping against
//! near-identical neighbors.
//!
//! Each test creates several atoms that share most lines (so the mock
//! bag-of-words embedder puts them above the neighbor similarity floor)
//! and each has a short unique tail. The strip resolver should return
//! content with the shared lines removed but the unique tail intact.

mod support;

use atomic_core::health::chunk_strip::strip_shared_chunks_atom;
use atomic_core::CreateAtomRequest;
use support::{setup_core, Backend, MockAiServer};

/// Shared preamble — appears verbatim in every atom so line-level matching
/// across neighbors flags every line here. Kept under the target chunk
/// size so every atom fits in one chunk (this is the case the original
/// chunk-hash filter can't handle and line-level strip can).
const SHARED_PREAMBLE: &str = "# App Support Record

## Runtime environment

Linux - General at I&H

## Health endpoints

| Environment | URL |
| --- | --- |
| Dev | https://dev.example.com/health |
| UAT | https://uat.example.com/health |
| Prod | https://prod.example.com/health |

## Troubleshooting outage alerts

| Alert message | Response |
| --- | --- |
| Unable to connect to database service | Check cluster. |
| Unable to connect to cache service | Check Redis cluster. |";

fn atom_body(unique_marker: &str) -> String {
    format!(
        "{preamble}\n\n## Customer\n\n{unique_marker} customer group with unique contacts and fleet size {unique_marker}.",
        preamble = SHARED_PREAMBLE,
        unique_marker = unique_marker,
    )
}

async fn make_atom(core: &atomic_core::AtomicCore, content: &str) -> String {
    core.create_atom(
        CreateAtomRequest {
            content: content.to_string(),
            ..Default::default()
        },
        |_| {},
    )
    .await
    .expect("create atom")
    .expect("atom inserted")
    .atom
    .id
}

#[tokio::test]
async fn strip_shared_chunks_removes_shared_lines() {
    let mock = MockAiServer::start().await;
    let handle = setup_core(Backend::Sqlite, &mock.base_url())
        .await
        .expect("harness");
    let core = &handle.core;

    // Seed 4 atoms. The strip resolver requires at least 2 neighbor matches
    // per line by default, so 4 siblings gives plenty of signal.
    let markers = ["AlphaService", "BravoService", "CharlieService", "DeltaService"];
    let mut ids = Vec::new();
    for marker in markers.iter() {
        let id = make_atom(core, &atom_body(marker)).await;
        ids.push(id);
    }

    let target = &ids[0];
    let (new_content, action) = strip_shared_chunks_atom(core, target, true)
        .await
        .expect("strip dry-run");

    assert!(action.is_none(), "dry_run=true must not produce a FixAction");

    let original = core
        .get_atom(target)
        .await
        .expect("get_atom")
        .expect("atom")
        .atom
        .content;

    assert_ne!(
        new_content, original,
        "expected strip to mutate content when neighbors share lines",
    );
    assert!(
        new_content.len() < original.len(),
        "expected shorter content after strip: orig_len={}, new_len={}\nnew:\n{}",
        original.len(),
        new_content.len(),
        new_content,
    );
    // Unique marker for this atom must survive.
    assert!(
        new_content.contains("AlphaService"),
        "target's unique marker must survive strip; got:\n{}",
        new_content,
    );
    // Shared table-skeleton row must be gone.
    assert!(
        !new_content.contains("| Environment | URL |"),
        "shared table header should be stripped; got:\n{}",
        new_content,
    );
}

#[tokio::test]
async fn strip_shared_chunks_is_noop_when_no_neighbors() {
    let mock = MockAiServer::start().await;
    let handle = setup_core(Backend::Sqlite, &mock.base_url())
        .await
        .expect("harness");
    let core = &handle.core;

    let content = "# Solo atom\n\nOnly atom in the KB — nothing to compare against.\n\n## Notes\n\nFull of unique content that has no near-identical neighbor in this database, so Strip must be a no-op.";
    let id = make_atom(core, content).await;

    let (new_content, action) = strip_shared_chunks_atom(core, &id, true)
        .await
        .expect("strip no-op");
    assert!(action.is_none());
    let original = core
        .get_atom(&id)
        .await
        .expect("get_atom")
        .expect("atom")
        .atom
        .content;
    assert_eq!(
        new_content, original,
        "solo atom must be returned unchanged — UI can show 'no boilerplate found'",
    );
}
#[tokio::test]
async fn strip_shared_chunks_auto_dismisses_boilerplate_pollution() {
    let mock = MockAiServer::start().await;
    let handle = setup_core(Backend::Sqlite, &mock.base_url())
        .await
        .expect("harness");
    let core = &handle.core;

    let markers = ["AlphaService", "BravoService", "CharlieService", "DeltaService"];
    let mut ids = Vec::new();
    for marker in markers.iter() {
        let id = make_atom(core, &atom_body(marker)).await;
        ids.push(id);
    }
    let target = ids[0].clone();

    // Pre-condition: no dismissal for this atom yet.
    let before = core
        .list_dismissed_keys("boilerplate_pollution")
        .await
        .expect("list dismissals");
    assert!(
        !before.iter().any(|(k, _)| k == &target),
        "target must not already be dismissed before strip",
    );

    // Apply (dry_run = false) so the strip commits and auto-dismisses.
    let (_new_content, action) = strip_shared_chunks_atom(core, &target, false)
        .await
        .expect("strip apply");
    assert!(action.is_some(), "apply must produce a FixAction");

    let after = core
        .list_dismissed_keys("boilerplate_pollution")
        .await
        .expect("list dismissals");
    assert!(
        after.iter().any(|(k, _)| k == &target),
        "strip must auto-dismiss the boilerplate_pollution entry for this atom so the review row doesn't flicker back after re-embed",
    );
}

