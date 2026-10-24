// The companion hooks (docs/design/extension-api.md, "Additive hooks") in a REAL window against a real
// gateway: the host reads the page for the augment provider, the review commands run the hooks around a
// real promote, and the Inbox shows a decorator's data.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { EscurelExtensionApi } from '../../../src/api/types';
import { uriForPage } from '../../../src/fs/provider';

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

suite('Companion hooks', () => {
  let api: EscurelApi & EscurelExtensionApi;
  let pageId: string;
  let skill: string;
  let original: string;

  suiteSetup(async () => {
    api = (await vscode.extensions.getExtension('datazoo.escurel')!.activate()) as EscurelApi &
      EscurelExtensionApi;
    const page = await api.services.client.listInstancesPage({ skill_id: 'customer', limit: 1 });
    pageId = page.instances[0]!.page_id;
    skill = 'customer';
    original = (await api.services.client.expand({ page_id: pageId, raw: true })).content!;
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    const cur = await api.services.client.expand({ page_id: pageId, raw: true });
    if (cur.content !== original && cur.content_sha256) {
      await api.services.client.updatePage({
        page_id: pageId,
        content: original,
        base_sha256: cur.content_sha256,
      });
    }
    for (const d of await api.services.client.listDrafts()) {
      if (d.status === 'open') {
        await api.services.client
          .discardDraft({ draft_id: d.draft_id, reason: 'hooks suite' })
          .catch(() => undefined);
      }
    }
  });

  test('the page augment provider is asked about the real page when its view opens', async () => {
    const asked: { pageId: string; skill: string; fm: Record<string, unknown> }[] = [];
    const reg = api.registerPageAugment(skill, {
      sections: (ctx) => {
        asked.push({ pageId: ctx.pageId, skill: ctx.skill, fm: ctx.frontmatter });
        return [{ id: 'extra', title: 'Extra', blocks: [{ type: 'text', text: 'from a companion' }] }];
      },
    });
    try {
      await vscode.commands.executeCommand('vscode.openWith', uriForPage(pageId), 'escurel.pageAsUi');
      const got = await until(() => asked[0]);
      assert.equal(got.pageId, pageId);
      assert.equal(got.skill, skill);
      assert.equal(typeof got.fm, 'object');
    } finally {
      reg.dispose();
    }
  });

  async function newDraft(): Promise<string> {
    const cur = await api.services.client.expand({ page_id: pageId, raw: true });
    const made = await api.services.client.createDraft({
      target_page_id: pageId,
      content: `${cur.content}\n\nChanged by the hooks suite.\n`,
      base_sha256: cur.content_sha256,
    });
    return made.draft.draft_id;
  }

  test('a veto from a review hook stops the accept; the draft stays open', async () => {
    const draftId = await newDraft();
    const seen: string[] = [];
    const reg = api.registerReviewHook({
      before: (ctx) => {
        seen.push(`${ctx.decision}:${ctx.drafts.map((d) => d.draftId).join(',')}`);
        return { veto: 'Not yet.' };
      },
      after: (ctx) => void seen.push(`after:${ctx.draftId}`),
    });
    try {
      await vscode.commands.executeCommand('escurel.promote', { draftId });
      const draft = (await api.services.client.listDrafts()).find((d) => d.draft_id === draftId);
      assert.equal(draft?.status, 'open', 'a vetoed accept must not promote');
      assert.deepEqual(seen, [`accepted:${draftId}`]);
    } finally {
      reg.dispose();
      await api.services.client.discardDraft({ draft_id: draftId, reason: 'hooks suite' });
    }
  });

  test('a hook that throws does not block the accept, and after hears the recorded decision', async () => {
    const draftId = await newDraft();
    const heard: { id: string; decision: string; author: string; note: string }[] = [];
    const bad = api.registerReviewHook({
      before: () => {
        throw new Error('boom');
      },
      after: () => {
        throw new Error('boom');
      },
    });
    const good = api.registerReviewHook({
      after: (ctx) =>
        void heard.push({
          id: ctx.draftId,
          decision: ctx.decision,
          author: ctx.provenance.author ?? '',
          note: ctx.note,
        }),
    });
    try {
      await vscode.commands.executeCommand('escurel.promote', { draftId });
      const page = await api.services.client.expand({ page_id: pageId, raw: true });
      assert.match(String(page.content), /Changed by the hooks suite/, 'the accept must go through');
      assert.equal(heard.length, 1);
      assert.equal(heard[0]!.id, draftId);
      assert.equal(heard[0]!.decision, 'accepted');
      assert.equal(heard[0]!.note, '');
      assert.ok(heard[0]!.author, 'the proposal provenance carries its author');
    } finally {
      bad.dispose();
      good.dispose();
    }
  });

  test('an Inbox decorator decorates the rows of its skill and sets the view data', async () => {
    const events = (await api.services.client.listInbox()).events;
    const target = events[0];
    assert.ok(target, 'the seed has an inbox event');
    const reg = api.registerInboxDecorator(target.label_skill, {
      decorate: (row) =>
        row.eventId === target.event_id ? { badge: 'Seen by hook', status: 'open' } : undefined,
      emptyMessage: { en: 'Nothing yet.', de: 'Noch nichts.' },
    });
    try {
      const rows = await api.inbox.getChildren();
      const row = rows.find((r) => r.kind === 'event' && r.event.event_id === target.event_id);
      assert.ok(row && row.kind === 'event');
      assert.ok(row.description.startsWith('Seen by hook'), row.description);
      assert.equal(row.status, 'open');
    } finally {
      reg.dispose();
    }
    const plain = await api.inbox.getChildren();
    const back = plain.find((r) => r.kind === 'event' && r.event.event_id === target.event_id);
    assert.ok(back && back.kind === 'event' && back.status === undefined, 'the decoration goes with the decorator');
  });
});
