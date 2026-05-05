import { useState } from 'react';
import { RefreshCw, Loader2, Check, Scissors, Sparkles, AlertCircle } from 'lucide-react';
import { applyFix, type BoilerplateEntry, type ItemStatus } from './types';
import { getTransport } from '../../../../lib/transport';
import { runReviewAction } from './reviewActions';
import { lineDiff } from './diffUtil';
import { toast } from '../../../../stores/toasts';

export interface BoilerplateAtomRowProps {
  atom: BoilerplateEntry;
  onResolved: (atomId: string) => void;
}

type StripMode = 'chunk' | 'llm';

interface StripPreview {
  mode: StripMode;
  original: string;
  proposed: string;
}

/** An LLM strip that returns unchanged content, or a deterministic chunk
 * strip that finds nothing shared, both land here. The UI must make clear
 * that no edit is pending so the user isn't staring at an empty diff. */
function isNoopPreview(p: StripPreview): boolean {
  return p.original.trim() === p.proposed.trim();
}

export function BoilerplateAtomRow({ atom, onResolved }: BoilerplateAtomRowProps) {
  const [status, setStatus] = useState<ItemStatus>('idle');
  const [preview, setPreview] = useState<StripPreview | null>(null);
  const [pending, setPending] = useState<StripMode | 'apply' | null>(null);

  const reembed = async () => {
    setStatus('saving');
    const ok = await applyFix('Re-embed atom', 'boilerplate_pollution', atom.id, { action: 'reembed' });
    if (ok === undefined) { setStatus('idle'); return; }
    setStatus('done');
    setTimeout(() => onResolved(atom.id), 400);
  };

  /** Deterministic strip: compare atom line-by-line against its near-
   * identical neighbors and remove lines that appear in >= N of them.
   * Never goes to the LLM, always produces the same answer for the same
   * DB state. The canonical fix for structural boilerplate (shared
   * headers, table skeletons) which LLM strip can't detect. */
  const previewChunkStrip = async () => {
    setPending('chunk');
    try {
      const current = await getTransport().invoke<{ content: string }>('get_atom', { id: atom.id });
      const resp = await getTransport().invoke<{ content: string }>('apply_health_item_fix', {
        check: 'boilerplate_pollution',
        item_id: atom.id,
        action: 'strip_shared_chunks',
        dry_run: true,
      });
      setPreview({ mode: 'chunk', original: current.content, proposed: resp.content });
    } catch (e) {
      toast.error('Preview strip failed', {
        detail: e instanceof Error ? e.message : String(e),
        retry: () => previewChunkStrip(),
      });
    } finally {
      setPending(null);
    }
  };

  /** LLM strip: asks the configured chat model to rewrite the atom with
   * boilerplate removed. Works well when boilerplate is literal phrasing;
   * often a no-op on runbook notes whose boilerplate is *structural*
   * (that's the chunk strip's job). */
  const previewLlmStrip = async () => {
    setPending('llm');
    try {
      const current = await getTransport().invoke<{ content: string }>('get_atom', { id: atom.id });
      const resp = await getTransport().invoke<{ content: string }>('health_strip_boilerplate', {
        atom_id: atom.id,
        dry_run: true,
      });
      setPreview({ mode: 'llm', original: current.content, proposed: resp.content });
    } catch (e) {
      toast.error('LLM strip preview failed', {
        detail: e instanceof Error ? e.message : String(e),
        retry: () => previewLlmStrip(),
      });
    } finally {
      setPending(null);
    }
  };

  const applyStrip = async () => {
    if (!preview) return;
    setPending('apply');
    const ok = preview.mode === 'chunk'
      ? await runReviewAction({
          label: 'Apply strip',
          command: 'apply_health_item_fix',
          args: {
            check: 'boilerplate_pollution',
            item_id: atom.id,
            action: 'strip_shared_chunks',
            dry_run: false,
          },
        })
      : await runReviewAction({
          label: 'Apply strip',
          command: 'health_strip_boilerplate',
          args: { atom_id: atom.id, dry_run: false },
        });
    if (ok === undefined) { setPending(null); return; }
    setStatus('done');
    setTimeout(() => onResolved(atom.id), 400);
  };

  const noop = preview !== null && isNoopPreview(preview);

  return (
    <div className="p-2.5 bg-[#1e1e1e] rounded border border-white/5">
      <div className="flex items-center gap-3">
        <div className="flex-1 min-w-0">
          <p className="text-xs text-gray-200 truncate">
            {atom.title || <span className="italic text-gray-500">Untitled atom</span>}
          </p>
          <p className="text-xs text-gray-600 mt-0.5">
            {atom.clone_count} near-identical edge{atom.clone_count !== 1 ? 's' : ''}
          </p>
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          <button
            type="button"
            onClick={previewChunkStrip}
            disabled={pending !== null || status === 'done'}
            className="px-2 py-1 rounded text-xs text-gray-400 hover:text-gray-200 bg-[#2a2a2a] border border-white/5 transition-colors disabled:opacity-40 inline-flex items-center gap-1"
            title="Remove lines this atom shares with near-identical neighbors (deterministic)"
          >
            {pending === 'chunk' ? <Loader2 className="w-3 h-3 animate-spin" /> : <Scissors className="w-3 h-3" />}
            Strip shared
          </button>
          <button
            type="button"
            onClick={previewLlmStrip}
            disabled={pending !== null || status === 'done'}
            className="px-2 py-1 rounded text-xs text-gray-400 hover:text-gray-200 bg-[#2a2a2a] border border-white/5 transition-colors disabled:opacity-40 inline-flex items-center gap-1"
            title="Ask LLM to rewrite the atom with template boilerplate removed"
          >
            {pending === 'llm' ? <Loader2 className="w-3 h-3 animate-spin" /> : <Sparkles className="w-3 h-3" />}
            LLM strip…
          </button>
          <button
            type="button"
            onClick={reembed}
            disabled={status === 'saving' || status === 'done' || pending !== null}
            className="px-2 py-1 rounded text-xs text-gray-400 hover:text-gray-200 bg-[#2a2a2a] border border-white/5 transition-colors disabled:opacity-40 inline-flex items-center gap-1"
            title="Reset the embedding — useful after editing content"
          >
            {status === 'saving'
              ? <Loader2 className="w-3 h-3 animate-spin" />
              : status === 'done'
                ? <Check className="w-3 h-3 text-green-500" />
                : <RefreshCw className="w-3 h-3" />}
            Re-embed
          </button>
        </div>
      </div>
      {preview && noop && (
        <div className="mt-2 space-y-2 border-t border-white/5 pt-2">
          <div className="flex items-start gap-2 rounded p-2 bg-yellow-900/10 border border-yellow-700/20">
            <AlertCircle className="w-3.5 h-3.5 text-yellow-400 mt-0.5 shrink-0" />
            <div className="text-xs text-yellow-200/90 leading-relaxed">
              {preview.mode === 'chunk'
                ? <>No lines in this atom are shared with enough near-identical neighbors to strip. The atom&rsquo;s boilerplate may be too unique to detect by line match &mdash; try <span className="font-semibold">LLM strip</span> or <span className="font-semibold">Re-embed</span> if the content has already been edited.</>
                : <>The LLM returned the atom unchanged &mdash; it couldn&rsquo;t identify template text to remove. Try <span className="font-semibold">Strip shared</span> for structural boilerplate (shared headers, tables) or <span className="font-semibold">Re-embed</span> if the content is already clean.</>}
            </div>
          </div>
          <div className="flex justify-end">
            <button
              type="button"
              onClick={() => setPreview(null)}
              className="px-2 py-1 rounded text-xs text-gray-400 hover:text-gray-200 bg-[#2a2a2a] border border-white/5"
            >
              Dismiss
            </button>
          </div>
        </div>
      )}
      {preview && !noop && (
        <div className="mt-2 space-y-2 border-t border-white/5 pt-2">
          <p className="text-xs text-yellow-300/80">
            Preview {preview.mode === 'chunk' ? '(deterministic)' : '(LLM)'} — apply to update the atom
          </p>
          <pre className="text-xs bg-[#161616] rounded p-2 max-h-72 overflow-y-auto whitespace-pre-wrap leading-relaxed font-sans">
            {lineDiff(preview.original, preview.proposed).map((p, i) => (
              <span key={i} className={
                p.type === 'insert' ? 'bg-green-900/30 text-green-300' :
                p.type === 'delete' ? 'bg-red-900/30 text-red-300' :
                'text-gray-400'
              }>{p.text}</span>
            ))}
          </pre>
          <div className="flex justify-end gap-1.5">
            <button
              type="button"
              onClick={() => setPreview(null)}
              className="px-2 py-1 rounded text-xs text-gray-400 hover:text-gray-200 bg-[#2a2a2a] border border-white/5"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={applyStrip}
              disabled={pending !== null}
              className="px-2 py-1 rounded text-xs text-white bg-purple-600 hover:bg-purple-500 disabled:opacity-40 inline-flex items-center gap-1"
            >
              {pending === 'apply' ? <Loader2 className="w-3 h-3 animate-spin" /> : null}
              Apply strip
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
