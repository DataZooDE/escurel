import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { uriForPage } from '../fs/provider';
import { findWikilinks } from '../skills/wikilinks';

interface PageItem extends vscode.QuickPickItem {
  pageId: string;
}

/** `escurel.search` (SPEC §3.8): a QuickPick over `search { granularity: page }`, placeholder as the mock's box. */
export async function searchCommand(client: () => EscurelClient): Promise<void> {
  const qp = vscode.window.createQuickPick<PageItem>();
  qp.placeholder = 'Search skills, instances, blocks…';
  qp.matchOnDescription = true;
  qp.matchOnDetail = true;
  let seq = 0;
  qp.onDidChangeValue(async (q) => {
    const my = ++seq;
    if (!q.trim()) return void (qp.items = []);
    qp.busy = true;
    try {
      const res = await client().search({ q, k: 20, granularity: 'page' });
      if (my !== seq) return;
      qp.items = res.hits.map((h) => ({
        label: `$(${h.page_type === 'skill' ? 'symbol-class' : 'symbol-field'}) ${h.slug ?? h.page_id}`,
        description: `${h.skill} · ${h.page_type}`,
        detail: h.snippet.replace(/\s+/g, ' ').slice(0, 160),
        pageId: h.page_id,
      }));
    } catch (e) {
      if (my === seq) qp.items = [{ label: '$(warning) ' + describeError(e), pageId: '' }];
    } finally {
      if (my === seq) qp.busy = false;
    }
  });
  qp.onDidAccept(() => {
    const pick = qp.selectedItems[0];
    qp.hide();
    if (pick?.pageId) void vscode.commands.executeCommand('escurel.openPage', pick.pageId);
  });
  qp.onDidHide(() => qp.dispose());
  qp.show();
}

/** `escurel.resolve`: the `[[skill::id]]` under the cursor (or the one picked), resolved and opened. */
export async function resolveCommand(
  client: () => EscurelClient,
  wikilink?: string,
): Promise<void> {
  let link = wikilink;
  if (!link) {
    const editor = vscode.window.activeTextEditor;
    if (editor) {
      const offset = editor.document.offsetAt(editor.selection.active);
      link = findWikilinks(editor.document.getText()).find(
        (l) => l.start <= offset && offset <= l.end,
      )?.text;
    }
  }
  if (!link) {
    link = await vscode.window.showInputBox({
      prompt: 'Wikilink to resolve',
      placeHolder: '[[skill::id]]',
    });
    if (!link) return;
  }
  try {
    const r = await client().resolve({ wikilink: link });
    if (!r.exists || !r.page)
      return void vscode.window.showWarningMessage(`escurel: ${link} resolves to no page`);
    await vscode.commands.executeCommand('escurel.openPage', r.page.page_id);
  } catch (e) {
    void vscode.window.showErrorMessage(`escurel: resolve failed — ${describeError(e)}`);
  }
}

/** Open a page the way its type wants: a skill as raw markdown, an instance as page-as-UI (falls back to raw until the editor lands). */
export async function openPage(pageId: string, raw = false): Promise<void> {
  const uri = uriForPage(pageId);
  if (pageId.startsWith('markdown/instances/') && !raw) {
    await vscode.commands.executeCommand('vscode.openWith', uri, 'escurel.pageAsUi');
    return;
  }
  await vscode.window.showTextDocument(uri, { preview: true });
}
