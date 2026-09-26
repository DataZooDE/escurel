import { describe, expect, it, vi } from 'vitest';
import { LoroDoc } from 'loro-crdt';
import { EscurelError, type Draft, type EscurelClient } from '../../src/client';
import {
  buildReplaceOpFromSnapshot,
  findOpenDraftForPage,
  writeInstanceDraft,
} from '../../src/fs/draftWrite';

describe('buildReplaceOpFromSnapshot', () => {
  it('op round-trip: replaces whole text from snapshot and updates the original document', () => {
    // 1. Establish the session document on the "gateway" with initial body.
    const originalDoc = new LoroDoc();
    const body = originalDoc.getText('body');
    body.insert(0, '# Customer Acme\n\nInitial notes on Acme Corp.\n');
    originalDoc.commit();

    // 2. The gateway exports a base64 snapshot on open_session.
    const snapshotBytes = originalDoc.export({ mode: 'snapshot' });
    const snapshotBase64 = Buffer.from(snapshotBytes).toString('base64');

    // 3. Editor edits markdown: build the replacement op against the snapshot.
    const nextMarkdown = '# Customer Acme\n\nUpdated notes with new revenue numbers.\n';
    const opBase64 = buildReplaceOpFromSnapshot(snapshotBase64, nextMarkdown);

    // 4. The gateway imports the base64 op sent via apply_op.
    const opBytes = Buffer.from(opBase64, 'base64');
    originalDoc.import(opBytes);

    // 5. Causal continuity holds: the original document reflects the new markdown.
    expect(originalDoc.getText('body').toString()).toBe(nextMarkdown);
  });

  it('an op built without the snapshot does not produce the markdown it meant to', () => {
    // Why the snapshot is not a convenience. Both ways of skipping it fail, and
    // neither announces itself — the gateway answers `ok` with an advanced
    // `merged_version` either way:
    //
    //   * rebuild the text locally and export from a LOCAL version, and the ops
    //     depend on history the session has never seen, so Loro holds them
    //     pending and the content does not move at all (observed against a live
    //     gateway: the draft came back byte-identical);
    //   * or start from an empty document, and the ops have no missing
    //     dependency and merge as a CONCURRENT insert — the session ends up
    //     holding both texts at once, which is the case below.
    const session = new LoroDoc();
    session.getText('body').insert(0, '# Initial Title\n\nOriginal body.\n');
    session.commit();

    const detached = new LoroDoc();
    detached.getText('body').insert(0, '# Overwrite Attempt\n');
    detached.commit();
    session.import(detached.export({ mode: 'update' }));

    const merged = session.getText('body').toString();
    expect(merged).not.toBe('# Overwrite Attempt\n');
    expect(merged).toContain('# Initial Title');
    expect(merged).toContain('# Overwrite Attempt');

    // Through the snapshot, the same intent lands exactly and nothing else survives.
    const fresh = new LoroDoc();
    fresh.getText('body').insert(0, '# Initial Title\n\nOriginal body.\n');
    fresh.commit();
    const snapshot = Buffer.from(fresh.export({ mode: 'snapshot' })).toString('base64');
    fresh.import(
      Buffer.from(buildReplaceOpFromSnapshot(snapshot, '# Overwrite Attempt\n'), 'base64'),
    );
    expect(fresh.getText('body').toString()).toBe('# Overwrite Attempt\n');
  });

  it('refuses a null snapshot rather than silently doing nothing', () => {
    expect(() => buildReplaceOpFromSnapshot(null, '# Hello')).toThrow(EscurelError);
    let thrown: unknown;
    try {
      buildReplaceOpFromSnapshot(null, '# Hello');
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(EscurelError);
    const err = thrown as EscurelError;
    expect(err.kind).toBe('refused');
    expect(err.message).toContain('cannot edit live');
  });

  it('refuses an undefined snapshot as well', () => {
    expect(() => buildReplaceOpFromSnapshot(undefined, '# Hello')).toThrow(EscurelError);
  });
});

describe('findOpenDraftForPage', () => {
  const targetPageId = 'markdown/instances/customer/acme.md';

  const makeDraft = (draftId: string, pageId: string, status: Draft['status']): Draft => ({
    draft_id: draftId,
    target_page_id: pageId,
    content: '---\nname: Test\n---\nBody\n',
    content_sha256: '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
    base_sha256: null,
    author: 'user-1',
    event_id: null,
    status,
    reason: null,
    decided_by: null,
    created_at: '2026-09-26T10:00:00Z',
    changeset_id: null,
    base_version: null,
    run_id: null,
    root_event_id: null,
  });

  it('reuses an existing open draft rather than creating a second one', () => {
    const drafts: Draft[] = [
      makeDraft('d-other', 'markdown/instances/customer/other.md', 'open'),
      makeDraft('d-existing', targetPageId, 'open'),
    ];

    const found = findOpenDraftForPage(drafts, targetPageId);
    expect(found).toBeDefined();
    expect(found?.draft_id).toBe('d-existing');
  });

  it('returns undefined when there is no draft at all', () => {
    expect(findOpenDraftForPage([], targetPageId)).toBeUndefined();
  });

  it('returns undefined when existing drafts for the page are already decided', () => {
    const drafts: Draft[] = [
      makeDraft('d-promoted', targetPageId, 'promoted'),
      makeDraft('d-discarded', targetPageId, 'discarded'),
      makeDraft('d-other', 'markdown/instances/customer/other.md', 'open'),
    ];

    expect(findOpenDraftForPage(drafts, targetPageId)).toBeUndefined();
  });
});

describe('writeInstanceDraft orchestration', () => {
  const pageId = 'markdown/instances/customer/acme.md';
  const newContent = '---\nname: Acme Corp\n---\nUpdated body\n';

  function createValidSnapshot(): string {
    const doc = new LoroDoc();
    const text = doc.getText('body');
    text.insert(0, '---\nname: Acme\n---\nOriginal body\n');
    doc.commit();
    return Buffer.from(doc.export({ mode: 'snapshot' })).toString('base64');
  }

  it('reuses existing open draft, opens session, applies op, and commits', async () => {
    const snapshot = createValidSnapshot();
    const existingDraft: Draft = {
      draft_id: 'd-existing-42',
      target_page_id: pageId,
      content: '---\nname: Acme\n---\nOriginal body\n',
      content_sha256: 'a'.repeat(64),
      base_sha256: null,
      author: 'user-me',
      event_id: null,
      status: 'open',
      reason: null,
      decided_by: null,
      created_at: '2026-09-26T10:00:00Z',
      changeset_id: null,
      base_version: null,
      run_id: null,
      root_event_id: null,
    };

    const mockClient = {
      listDrafts: vi.fn().mockResolvedValue([existingDraft]),
      createDraft: vi.fn(),
      openSession: vi.fn().mockResolvedValue({
        session: 'sess-1',
        head_version: '0@0',
        ws_url: 'ws://localhost/crdt/ws/sess-1',
        snapshot,
      }),
      applyOp: vi.fn().mockResolvedValue({ ok: true, merged_version: '1@1' }),
      closeSession: vi.fn().mockResolvedValue({ ok: true, final_version: '1@1', issues: [] }),
    } as unknown as EscurelClient;

    const res = await writeInstanceDraft(mockClient, pageId, newContent);

    expect(res.draftId).toBe('d-existing-42');
    expect(mockClient.createDraft).not.toHaveBeenCalled();
    expect(mockClient.openSession).toHaveBeenCalledWith({ draft_id: 'd-existing-42' });
    expect(mockClient.applyOp).toHaveBeenCalledWith(
      expect.objectContaining({ session: 'sess-1', op: expect.any(String) }),
    );
    expect(mockClient.closeSession).toHaveBeenCalledWith({ session: 'sess-1', commit: true });
  });

  it('creates draft when none is open, then opens session, applies op, and commits', async () => {
    const snapshot = createValidSnapshot();
    const createdDraft: Draft = {
      draft_id: 'd-new-99',
      target_page_id: pageId,
      content: newContent,
      content_sha256: 'b'.repeat(64),
      base_sha256: 'c'.repeat(64),
      author: 'user-me',
      event_id: null,
      status: 'open',
      reason: null,
      decided_by: null,
      created_at: '2026-09-26T10:00:00Z',
      changeset_id: null,
      base_version: null,
      run_id: null,
      root_event_id: null,
    };

    const mockClient = {
      listDrafts: vi.fn().mockResolvedValue([]),
      createDraft: vi.fn().mockResolvedValue({ ok: true, draft: createdDraft }),
      openSession: vi.fn().mockResolvedValue({
        session: 'sess-2',
        head_version: '0@0',
        ws_url: 'ws://localhost/crdt/ws/sess-2',
        snapshot,
      }),
      applyOp: vi.fn().mockResolvedValue({ ok: true, merged_version: '1@1' }),
      closeSession: vi.fn().mockResolvedValue({ ok: true, final_version: '1@1', issues: [] }),
    } as unknown as EscurelClient;

    const res = await writeInstanceDraft(mockClient, pageId, newContent, 'c'.repeat(64));

    expect(res.draftId).toBe('d-new-99');
    expect(mockClient.createDraft).toHaveBeenCalledWith({
      target_page_id: pageId,
      content: newContent,
      base_sha256: 'c'.repeat(64),
    });
    expect(mockClient.openSession).toHaveBeenCalledWith({ draft_id: 'd-new-99' });
    expect(mockClient.applyOp).toHaveBeenCalled();
    expect(mockClient.closeSession).toHaveBeenCalledWith({ session: 'sess-2', commit: true });
  });

  it('discards session if op application fails to avoid locking the draft session slot', async () => {
    const snapshot = createValidSnapshot();
    const mockClient = {
      listDrafts: vi.fn().mockResolvedValue([]),
      createDraft: vi.fn().mockResolvedValue({
        ok: true,
        draft: { draft_id: 'd-fail', status: 'open' },
      }),
      openSession: vi.fn().mockResolvedValue({
        session: 'sess-err',
        head_version: '0@0',
        ws_url: 'ws://localhost/crdt/ws/sess-err',
        snapshot,
      }),
      applyOp: vi.fn().mockRejectedValue(new Error('apply_op failed')),
      closeSession: vi.fn().mockResolvedValue({ ok: true }),
    } as unknown as EscurelClient;

    await expect(writeInstanceDraft(mockClient, pageId, newContent)).rejects.toThrow(
      'apply_op failed',
    );
    // Cleanup ensures commit: false was sent to release the gateway session
    expect(mockClient.closeSession).toHaveBeenCalledWith({ session: 'sess-err', commit: false });
  });
});

describe('writeInstanceDraft: losing the create race', () => {
  it('adopts the draft that appeared between the read and the create', async () => {
    // A page carries at most one open draft, so a create that loses this race is
    // refused with `conflict`. The draft that now exists is the one this save
    // belongs in — the user never called `create_draft` and must not see it.
    const raced: Draft = {
      draft_id: 'd-raced',
      target_page_id: 'markdown/instances/customer/acme.md',
      content: '# Acme\n',
      content_sha256: 'abc',
      base_sha256: null,
      author: 'ada',
      event_id: null,
      changeset_id: null,
      status: 'open',
      reason: null,
      decided_by: null,
      created_at: '2026-09-26T10:00:00Z',
      base_version: null,
      run_id: null,
      root_event_id: null,
    };
    const doc = new LoroDoc();
    doc.getText('body').insert(0, '# Acme\n');
    doc.commit();
    const snapshot = Buffer.from(doc.export({ mode: 'snapshot' })).toString('base64');

    const listDrafts = vi.fn().mockResolvedValueOnce([]).mockResolvedValueOnce([raced]);
    const client = {
      listDrafts,
      createDraft: vi.fn().mockRejectedValue(new EscurelError('conflict', 'already open')),
      openSession: vi.fn().mockResolvedValue({ session: 's1', head_version: 'v1', snapshot }),
      applyOp: vi.fn().mockResolvedValue({ ok: true, merged_version: 'v2' }),
      closeSession: vi.fn().mockResolvedValue({ ok: true, final_version: 'v2', issues: [] }),
    } as unknown as EscurelClient;

    const res = await writeInstanceDraft(client, raced.target_page_id, '# Acme edited\n');
    expect(res.draftId).toBe('d-raced');
    expect(client.openSession).toHaveBeenCalledWith({ draft_id: 'd-raced' });
    expect(client.closeSession).toHaveBeenCalledWith({ session: 's1', commit: true });
  });

  it('lets a refusal that is not a conflict through', async () => {
    const client = {
      listDrafts: vi.fn().mockResolvedValue([]),
      createDraft: vi.fn().mockRejectedValue(new EscurelError('forbidden', 'not yours')),
      openSession: vi.fn(),
    } as unknown as EscurelClient;
    await expect(
      writeInstanceDraft(client, 'markdown/instances/customer/acme.md', '# x\n'),
    ).rejects.toThrow(EscurelError);
    expect(client.openSession).not.toHaveBeenCalled();
  });
});
