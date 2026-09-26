// SPEC §8 M2 asks for "human live drafts in the same queue", and §7 PR-1 for a
// mutable personal draft of an instance landed by the promote path. This walks it
// in a real editor against a real gateway: edit the instance, the page does not
// move, the draft is in the queue, promote lands it.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
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

suite('M2 human drafts', () => {
  let api: EscurelApi;
  let pageId: string;
  let original: string;

  suiteSetup(async () => {
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
    const page = await api.services.client.listInstancesPage({ skill_id: 'customer', limit: 1 });
    pageId = page.instances[0]!.page_id;
    const exp = await api.services.client.expand({ page_id: pageId, raw: true });
    original = exp.content!;
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    // Put the page back, whichever way the tests left it.
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
          .discardDraft({ draft_id: d.draft_id, reason: 'drafts suite' })
          .catch(() => undefined);
      }
    }
  });

  test('editing an instance is held as a personal draft, and the page does not move', async () => {
    const uri = uriForPage(pageId);
    const doc = await vscode.workspace.openTextDocument(uri);
    assert.equal(doc.getText(), original, 'the editor must open the stored bytes');

    const edited = `${original}\n\nEdited by a human in the workbench.\n`;
    const edit = new vscode.WorkspaceEdit();
    edit.replace(uri, new vscode.Range(0, 0, doc.lineCount, 0), edited);
    assert.ok(await vscode.workspace.applyEdit(edit), 'the instance must be writable');
    assert.ok(await doc.save(), 'the save must succeed');

    // Held, not written: the page is exactly as it was.
    const page = await api.services.client.expand({ page_id: pageId, raw: true });
    assert.equal(page.content, original, 'an instance edit must not write the page');

    // …and the bytes are in a draft for that page.
    const draft = await until(async () => {
      const drafts = await api.services.client.listDrafts();
      return drafts.find((d) => d.status === 'open' && d.target_page_id === pageId);
    });
    assert.equal(draft.content, edited, 'the draft must hold exactly what was saved');

    // The queue shows it, with no manual refresh: this is the live path again.
    const rows = await until(async () => {
      const items = await api.awaiting.getChildren();
      return items.some((i) => 'id' in i && i.id === draft.draft_id) ? items : undefined;
    });
    assert.ok(
      rows.some((i) => 'id' in i && i.id === draft.draft_id),
      'the personal draft must appear in Awaiting you',
    );
  });

  test('after the save the editor is clean, and reopening shows the held work', async () => {
    // Found in a real window: an instance save lands in a draft, so the page never
    // changes — and an editor that re-read the page found its buffer different from
    // the file it had just saved and stayed dirty for ever, prompting on every close
    // to save changes that were already safe.
    const uri = uriForPage(pageId);
    const doc = await vscode.workspace.openTextDocument(uri);
    assert.equal(doc.isDirty, false, 'the saved document must not still be dirty');

    const draft = await until(async () => {
      const drafts = await api.services.client.listDrafts();
      return drafts.find((d) => d.status === 'open' && d.target_page_id === pageId);
    });
    const reread = Buffer.from(await vscode.workspace.fs.readFile(uri)).toString('utf8');
    assert.equal(reread, draft.content, 'a reopened instance must show the work in progress');
    const stat = await vscode.workspace.fs.stat(uri);
    assert.equal(
      stat.size,
      Buffer.byteLength(draft.content),
      'the reported size must describe what readFile returns',
    );
  });

  test('saving again edits the same draft rather than opening a second one', async () => {
    const uri = uriForPage(pageId);
    const doc = await vscode.workspace.openTextDocument(uri);
    const before = await until(async () => {
      const drafts = await api.services.client.listDrafts();
      return drafts.find((d) => d.status === 'open' && d.target_page_id === pageId);
    });

    const again = `${original}\n\nSecond pass by the same human.\n`;
    const edit = new vscode.WorkspaceEdit();
    edit.replace(uri, new vscode.Range(0, 0, doc.lineCount, 0), again);
    assert.ok(await vscode.workspace.applyEdit(edit));
    assert.ok(await doc.save());

    const open = (await api.services.client.listDrafts()).filter(
      (d) => d.status === 'open' && d.target_page_id === pageId,
    );
    assert.equal(open.length, 1, 'a page must never carry two open drafts');
    assert.equal(open[0]!.draft_id, before.draft_id, 'the same draft must be edited again');
    assert.equal(open[0]!.content, again, 'the draft must hold the newer bytes');
  });

  test('promoting the personal draft lands exactly those bytes on the page', async () => {
    const draft = await until(async () => {
      const drafts = await api.services.client.listDrafts();
      return drafts.find((d) => d.status === 'open' && d.target_page_id === pageId);
    });
    const promoted = await api.services.client.promoteDraft({ draft_id: draft.draft_id });
    assert.equal(promoted.ok, true, 'promote must succeed');

    const page = await until(async () => {
      const exp = await api.services.client.expand({ page_id: pageId, raw: true });
      return exp.content === draft.content ? exp : undefined;
    });
    assert.equal(page.content, draft.content, 'the page must now hold the drafted bytes');
  });
});
