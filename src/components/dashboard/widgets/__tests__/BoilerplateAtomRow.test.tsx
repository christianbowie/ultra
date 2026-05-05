import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BoilerplateAtomRow } from '../review/BoilerplateAtomRow';

const invoke = vi.fn();
vi.mock('../../../../lib/transport', () => ({
  getTransport: () => ({ invoke }),
}));

const CHUNK_STRIP_TITLE = 'Remove lines this atom shares with near-identical neighbors (deterministic)';
const LLM_STRIP_TITLE = 'Ask LLM to rewrite the atom with template boilerplate removed';

describe('BoilerplateAtomRow', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({ status: 'ok' });
  });

  afterEach(() => { cleanup(); });

  it('triggers re-embed', async () => {
    const onResolved = vi.fn();
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a1', title: 'x', clone_count: 3 }} onResolved={onResolved} />);
    await user.click(screen.getByText('Re-embed'));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('apply_health_item_fix', expect.objectContaining({
      check: 'boilerplate_pollution',
      item_id: 'a1',
      action: 'reembed',
    })));
    await waitFor(() => expect(onResolved).toHaveBeenCalledWith('a1'), { timeout: 1000 });
  });

  it('Strip shared: previews deterministic diff via apply_health_item_fix', async () => {
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'get_atom') return Promise.resolve({ content: 'Line A\nLine B\nLine C' });
      if (cmd === 'apply_health_item_fix' && args.action === 'strip_shared_chunks' && args.dry_run === true) {
        return Promise.resolve({ content: 'Line A' });
      }
      return Promise.resolve({ status: 'ok' });
    });
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a2', title: 'Test', clone_count: 2 }} onResolved={vi.fn()} />);
    await user.click(screen.getByTitle(CHUNK_STRIP_TITLE));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        'apply_health_item_fix',
        expect.objectContaining({
          check: 'boilerplate_pollution',
          item_id: 'a2',
          action: 'strip_shared_chunks',
          dry_run: true,
        }),
      ),
    );
    await waitFor(() => screen.getByText(/Preview \(deterministic\)/));
    expect(screen.getByText('Apply strip')).toBeTruthy();
    expect(screen.getByText('Cancel')).toBeTruthy();
  });

  it('LLM strip: previews via health_strip_boilerplate', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_atom') return Promise.resolve({ content: 'Original content here' });
      if (cmd === 'health_strip_boilerplate') return Promise.resolve({ content: 'Stripped content here' });
      return Promise.resolve({ status: 'ok' });
    });
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a3', title: 'Test', clone_count: 2 }} onResolved={vi.fn()} />);
    await user.click(screen.getByTitle(LLM_STRIP_TITLE));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('health_strip_boilerplate', expect.objectContaining({ atom_id: 'a3', dry_run: true })),
    );
    await waitFor(() => screen.getByText(/Preview \(LLM\)/));
    expect(screen.getByText('Apply strip')).toBeTruthy();
  });

  it('Shows no-op explainer when deterministic strip returns unchanged content', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_atom') return Promise.resolve({ content: 'Same content' });
      if (cmd === 'apply_health_item_fix') return Promise.resolve({ content: 'Same content' });
      return Promise.resolve({ status: 'ok' });
    });
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a4', title: 'Solo', clone_count: 1 }} onResolved={vi.fn()} />);
    await user.click(screen.getByTitle(CHUNK_STRIP_TITLE));
    await waitFor(() => screen.getByText(/No lines in this atom are shared/));
    // Apply button is hidden on no-op — only Dismiss shown.
    expect(screen.queryByText('Apply strip')).toBeNull();
    expect(screen.getByText('Dismiss')).toBeTruthy();
  });

  it('Shows no-op explainer when LLM strip returns identical content', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_atom') return Promise.resolve({ content: 'Subject-specific content' });
      if (cmd === 'health_strip_boilerplate') return Promise.resolve({ content: 'Subject-specific content' });
      return Promise.resolve({ status: 'ok' });
    });
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a5', title: 'Test', clone_count: 3 }} onResolved={vi.fn()} />);
    await user.click(screen.getByTitle(LLM_STRIP_TITLE));
    await waitFor(() => screen.getByText(/The LLM returned the atom unchanged/));
    expect(screen.queryByText('Apply strip')).toBeNull();
  });

  it('Cancel button hides the strip preview', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_atom') return Promise.resolve({ content: 'Original' });
      if (cmd === 'health_strip_boilerplate') return Promise.resolve({ content: 'Stripped' });
      return Promise.resolve({ status: 'ok' });
    });
    const user = userEvent.setup();
    render(<BoilerplateAtomRow atom={{ id: 'a6', title: 'Test', clone_count: 1 }} onResolved={vi.fn()} />);
    await user.click(screen.getByTitle(LLM_STRIP_TITLE));
    await waitFor(() => screen.getByText('Cancel'));
    await user.click(screen.getByText('Cancel'));
    expect(screen.queryByText('Apply strip')).toBeNull();
  });
});
