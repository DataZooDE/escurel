import { afterEach, describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import type { Draft } from '../../src/client';
import { ReviewController } from '../../src/review/controller';
import { ReviewHookRegistry } from '../../src/review/hooks';

const draft = (id: string, page: string, cs: string | null = null): Draft => ({
  draft_id: id,
  target_page_id: page,
  content: 'c',
  content_sha256: 'x',
  base_sha256: null,
  author: 'agent:planner',
  event_id: null,
  status: 'open',
  reason: null,
  decided_by: null,
  created_at: '2026-01-01T00:00:00Z',
  changeset_id: cs,
  base_version: null,
  run_id: 'run1',
  root_event_id: 'root1',
});

function setup(hooked = true) {
  const calls: [string, unknown][] = [];
  const drafts = [
    draft('d1', 'markdown/instances/acme-thing/a.md'),
    draft('d2', 'markdown/instances/acme-thing/b.md', 'cs1'),
    draft('d3', 'markdown/instances/acme-thing/c.md', 'cs1'),
  ];
  const client = {
    listDrafts: async () => drafts,
    promoteDraft: async (a: unknown) => (calls.push(['promoteDraft', a]), { ok: true }),
    discardDraft: async (a: unknown) => (calls.push(['discardDraft', a]), { ok: true }),
    promoteChangeset: async (a: unknown) => (
      calls.push(['promoteChangeset', a]),
      {
        results: [
          { draft_id: 'd2', page_id: 'markdown/instances/acme-thing/b.md', ok: true },
          { draft_id: 'd3', page_id: 'markdown/instances/acme-thing/c.md', ok: true, already_applied: true },
        ],
      }
    ),
    discardChangeset: async (a: unknown) => (calls.push(['discardChangeset', a]), { ok: true }),
    callTool: async () => ({}),
  };
  const hooks = new ReviewHookRegistry();
  const controller = new ReviewController(
    (() => client) as never,
    { setDraft() {}, getDraft: () => undefined } as never,
    { loadCommentsForDraft: async () => undefined } as never,
    () => undefined,
    hooked ? { hooks, gateway: client as never } : undefined,
  );
  return { controller, hooks, calls };
}

afterEach(() => vi.restoreAllMocks());

describe('review decision hooks in the controller', () => {
  it('a veto stops the decision and tells the reviewer why', async () => {
    const warn = vi.spyOn(vscode.window, 'showWarningMessage');
    const { controller, hooks, calls } = setup();
    hooks.register({ before: () => ({ veto: 'The impact is not reviewed yet.' }) });
    await controller.promote({ draftId: 'd1' });
    expect(calls).toEqual([]);
    expect(warn).toHaveBeenCalledWith('The impact is not reviewed yet.');
  });

  it('asks for a required note on accept, refuses an empty one, and hands the note to after', async () => {
    const input = vi.spyOn(vscode.window, 'showInputBox').mockResolvedValue('looks right');
    const { controller, hooks, calls } = setup();
    const seen: unknown[] = [];
    hooks.register({
      before: () => ({ note: 'required', notePrompt: 'Why accept?' }),
      after: (c) => void seen.push({ ...c, client: undefined }),
    });
    await controller.promote({ draftId: 'd1' });
    const opts = input.mock.calls[0]![0]!;
    expect(opts.prompt).toBe('Why accept?');
    expect(opts.validateInput!('  ')).toBeTruthy();
    expect(opts.validateInput!('x')).toBeFalsy();
    expect(calls).toEqual([['promoteDraft', { draft_id: 'd1' }]]);
    expect(seen).toEqual([
      {
        draftId: 'd1',
        targetPageId: 'markdown/instances/acme-thing/a.md',
        decision: 'accepted',
        note: 'looks right',
        provenance: expect.objectContaining({ author: 'agent:planner', run_id: 'run1', root_event_id: 'root1' }),
        client: undefined,
      },
    ]);
  });

  it('cancelling the note prompt cancels the decision', async () => {
    vi.spyOn(vscode.window, 'showInputBox').mockResolvedValue(undefined);
    const { controller, hooks, calls } = setup();
    hooks.register({ before: () => ({ note: 'optional' }) });
    await controller.promote({ draftId: 'd1' });
    expect(calls).toEqual([]);
  });

  it('asks nothing and records nothing without a hook that wants it', async () => {
    const input = vi.spyOn(vscode.window, 'showInputBox');
    const { controller, calls } = setup();
    await controller.promote({ draftId: 'd1' });
    expect(input).not.toHaveBeenCalled();
    expect(calls).toEqual([['promoteDraft', { draft_id: 'd1' }]]);
  });

  it('a hook that throws never blocks the decision', async () => {
    const { controller, hooks, calls } = setup();
    hooks.register({
      before: () => {
        throw new Error('boom');
      },
      after: () => {
        throw new Error('boom');
      },
    });
    await controller.promote({ draftId: 'd1' });
    expect(calls).toEqual([['promoteDraft', { draft_id: 'd1' }]]);
  });

  it('a reject uses its reason box as the note, required when a hook says so', async () => {
    const input = vi.spyOn(vscode.window, 'showInputBox').mockResolvedValue('wrong customer');
    const { controller, hooks, calls } = setup();
    const seen: { decision: string; note: string }[] = [];
    hooks.register({
      before: () => ({ note: 'required' }),
      after: (c) => void seen.push({ decision: c.decision, note: c.note }),
    });
    await controller.discard({ draftId: 'd1' });
    expect(input.mock.calls[0]![0]!.validateInput!('')).toBeTruthy();
    expect(calls).toEqual([['discardDraft', { draft_id: 'd1', reason: 'wrong customer' }]]);
    expect(seen).toEqual([{ decision: 'rejected', note: 'wrong customer' }]);
  });

  it('a reject keeps its optional reason box when no hook asks for more', async () => {
    const input = vi.spyOn(vscode.window, 'showInputBox').mockResolvedValue('');
    const { controller, calls } = setup();
    await controller.discard({ draftId: 'd1' });
    expect(input.mock.calls[0]![0]!.prompt).toMatch(/optional/);
    expect(calls).toEqual([['discardDraft', { draft_id: 'd1', reason: undefined }]]);
  });

  it('a changeset accept tells after about the drafts that were applied now, not the already applied', async () => {
    const { controller, hooks } = setup();
    const seen: string[] = [];
    let before: string[] = [];
    hooks.register({
      before: (c) => void (before = c.drafts.map((d) => d.draftId)),
      after: (c) => void seen.push(c.draftId),
    });
    await controller.promote({ changesetId: 'cs1' });
    expect(before.sort()).toEqual(['d2', 'd3']);
    expect(seen).toEqual(['d2']);
  });

  it('a changeset reject tells after about every open draft of the set', async () => {
    vi.spyOn(vscode.window, 'showInputBox').mockResolvedValue('');
    const { controller, hooks } = setup();
    const seen: string[] = [];
    hooks.register({ after: (c) => void seen.push(`${c.draftId}:${c.decision}`) });
    await controller.discard({ changesetId: 'cs1' });
    expect(seen.sort()).toEqual(['d2:rejected', 'd3:rejected']);
  });

  it('an accept with edits is reported as such when the companion says so', async () => {
    const { controller, hooks } = setup();
    const seen: string[] = [];
    hooks.register({ after: (c) => void seen.push(c.decision) });
    await controller.promote({ draftId: 'd1', withEdits: true });
    expect(seen).toEqual(['accepted_with_edits']);
  });

  it('does not report a decision that failed', async () => {
    const { controller, hooks } = setup();
    const seen: string[] = [];
    hooks.register({ after: (c) => void seen.push(c.draftId) });
    (controller as unknown as { client: () => { promoteDraft: () => Promise<never> } }).client = () => ({
      listDrafts: async () => [],
      promoteDraft: () => Promise.reject(new Error('conflict')),
    });
    await controller.promote({ draftId: 'd1' });
    expect(seen).toEqual([]);
  });
});
