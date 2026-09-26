// SPEC §8 M2 "done when" ends "…and the Awaiting count updates live". Nothing here
// calls refresh: the assertion is that the view is told to refetch on its own,
// because an event reached the extension over the socket.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until<T>(f: () => Promise<T | undefined> | T | undefined, ms = 20_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined) return v;
    if (Date.now() > end) throw new Error('timed out');
    await wait(200);
  }
}

suite('M2 live', () => {
  let api: EscurelApi;
  const opened: string[] = [];

  suiteSetup(async () => {
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
  });

  suiteTeardown(async () => {
    // Leave the queue as it was found, so a later suite does not inherit drafts.
    for (const draftId of opened) {
      try {
        await api.services.client.discardDraft({ draft_id: draftId, reason: 'live suite' });
      } catch {
        // Already decided by an assertion above; nothing to undo.
      }
    }
  });

  test('a draft created by anyone refreshes Awaiting with nobody calling refresh', async () => {
    // Arm the listener BEFORE the draft exists, or the signal it is waiting for
    // has already gone by.
    let fired = 0;
    const sub = api.awaiting.onDidChangeTreeData(() => {
      fired += 1;
    });

    // The socket may still be connecting; its first connect refreshes both views
    // to cover the replay gap, and counting that as the live signal would make
    // this test pass without any event at all.
    await wait(2_000);
    const baseline = fired;

    // Without an open socket nothing below could be a live update, and a passing
    // assertion would be measuring some other refresh.
    assert.equal(
      api.live.socketState,
      'open',
      'the live socket must be connected for this test to mean anything',
    );

    const page = await api.services.client.listInstancesPage({ skill_id: 'contact', limit: 1 });
    const target = page.instances[0]!.page_id;
    const exp = await api.services.client.expand({ page_id: target, raw: true });
    const created = await api.services.client.createDraft({
      target_page_id: target,
      content: `${exp.content}\n\nLive-update probe.\n`,
      base_sha256: exp.content_sha256,
      new_changeset: true,
    });
    opened.push(created.draft.draft_id);

    try {
      await until(async () => (fired > baseline ? fired : undefined));
    } finally {
      sub.dispose();
    }

    // …and the row the refetch produces is the new changeset.
    const changesetId = created.draft.changeset_id;
    assert.ok(changesetId, 'the probe draft must have a changeset');
    const rows = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.some((i) => 'id' in i && i.id === changesetId) ? items : undefined;
    });
    assert.ok(
      rows.some((i) => 'id' in i && i.id === changesetId),
      'the live refresh must surface the new changeset',
    );
  });

  test('deciding a draft elsewhere refreshes Awaiting with nobody calling refresh', async () => {
    const page = await api.services.client.listInstancesPage({ skill_id: 'engagement', limit: 1 });
    const target = page.instances[0]!.page_id;
    const exp = await api.services.client.expand({ page_id: target, raw: true });
    const created = await api.services.client.createDraft({
      target_page_id: target,
      content: `${exp.content}\n\nDecided-elsewhere probe.\n`,
      base_sha256: exp.content_sha256,
    });
    const draftId = created.draft.draft_id;
    await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.some((i) => 'id' in i && i.id === draftId) ? true : undefined;
    });

    let fired = 0;
    const sub = api.awaiting.onDidChangeTreeData(() => {
      fired += 1;
    });
    // A decision taken outside this window: the queue must shrink unannounced,
    // which is the mirror of it growing unannounced.
    await api.services.client.discardDraft({ draft_id: draftId, reason: 'live suite' });
    try {
      await until(async () => (fired > 0 ? fired : undefined));
    } finally {
      sub.dispose();
    }

    const gone = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.every((i) => !('id' in i) || i.id !== draftId) ? { items } : undefined;
    });
    assert.ok(
      !gone.items.some((i) => 'id' in i && i.id === draftId),
      'the discarded draft must leave the queue',
    );
  });
});
