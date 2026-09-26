// SPEC §8 M1 "done when": sign in (none mode here), browse skills →
// instances, open an instance as UI and raw, edit and save a skill with
// diagnostics, find a page by search.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until<T>(f: () => Promise<T | undefined> | T | undefined, ms = 15_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && !(Array.isArray(v) && v.length === 0)) return v;
    if (Date.now() > end) throw new Error('timed out');
    await wait(200);
  }
}

suite('M1', () => {
  let api: EscurelApi;
  suiteSetup(async () => {
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
    assert.equal(
      vscode.workspace.getConfiguration('escurel').get('gatewayUrl'),
      process.env.ESCUREL_TEST_GATEWAY,
    );
  });

  test('no issuer configured: the client talks to the gateway without a token', async () => {
    const skills = await api.services.client.listSkills();
    assert.ok(
      skills.some((s) => s.id === 'customer'),
      skills.map((s) => s.id).join(','),
    );
  });

  test('Knowledge: skills → instances, instances paged', async () => {
    const roots = await api.knowledge.getChildren();
    const customer = roots.find((n) => n.kind === 'skill' && n.label === 'customer');
    assert.ok(customer, 'customer skill row');
    const children = await api.knowledge.getChildren(customer);
    assert.ok(
      children.some((n) => n.kind === 'instance'),
      'instance rows',
    );
    const item = api.knowledge.getTreeItem(customer!);
    assert.equal(item.contextValue, 'skill');
  });

  test('a skill opens as raw markdown, an edit saves under the hash guard, and validate reports diagnostics', async () => {
    const uri = vscode.Uri.parse('escurel:/skills/customer.md');
    const doc = await vscode.workspace.openTextDocument(uri);
    const original = doc.getText();
    assert.ok(original.startsWith('---\n'), 'raw frontmatter');
    const editor = await vscode.window.showTextDocument(doc);
    // A bad render hint → a warning diagnostic from `validate`.
    await editor.edit((b) =>
      b.insert(new vscode.Position(1, 0), 'fields:\n  - {name: zz, render: sparkle}\n'),
    );
    const diags = await until(() =>
      vscode.languages.getDiagnostics(uri).filter((d) => d.code === 'field_render_unknown'),
    );
    assert.equal(diags[0]!.severity, vscode.DiagnosticSeverity.Warning);
    // Revert to the original plus a harmless line and save: the gateway takes it.
    await editor.edit((b) =>
      b.replace(new vscode.Range(0, 0, doc.lineCount, 0), original + '\nEdited from VS Code.\n'),
    );
    assert.ok(await doc.save(), 'save accepted');
    const back = await api.services.client.expand({
      page_id: 'markdown/skills/customer.md',
      raw: true,
    });
    assert.ok(back.content?.includes('Edited from VS Code.'), 'the gateway stored the edit');
    // Put it back.
    await editor.edit((b) => b.replace(new vscode.Range(0, 0, doc.lineCount, 0), original));
    await doc.save();
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });

  // Instances were read-only until the personal-draft write path landed (SPEC §7
  // PR-1). They are writable now, and a save is HELD as a draft rather than
  // written to the page — which `test/integration/suite/m2drafts.test.ts` walks
  // end to end. What stays true here is that Show Markdown opens the stored bytes.
  test('an instance opens as page-as-UI, and Show Markdown opens its markdown', async () => {
    const page = (await api.services.client.listInstancesPage({ skill_id: 'customer', limit: 1 }))
      .instances[0]!;
    await vscode.commands.executeCommand('escurel.openInstance', page.page_id);
    const tab = await until(() => {
      const t = vscode.window.tabGroups.activeTabGroup.activeTab;
      return t?.input instanceof vscode.TabInputCustom ? t : undefined;
    });
    assert.equal((tab.input as vscode.TabInputCustom).viewType, 'escurel.pageAsUi');
    await vscode.commands.executeCommand('escurel.showRaw', page.page_id);
    const doc = await until(() => vscode.window.activeTextEditor?.document);
    assert.equal(doc.uri.scheme, 'escurel');
    const stat = await vscode.workspace.fs.stat(doc.uri);
    assert.equal(stat.permissions, undefined, 'an instance is editable through its draft');
    const stored = await api.services.client.expand({ page_id: page.page_id, raw: true });
    assert.equal(doc.getText(), stored.content, 'Show Markdown shows the stored bytes');
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });

  test('search finds a page and resolve opens a wikilink target', async () => {
    const hits = await api.services.client.search({ q: 'customer', k: 5, granularity: 'page' });
    assert.ok(hits.hits.length > 0);
    const page = (await api.services.client.listInstancesPage({ skill_id: 'customer', limit: 1 }))
      .instances[0]!;
    const slug = String(
      page.frontmatter.id ??
        page.page_id
          .split('/')
          .pop()!
          .replace(/\.md$/, '')
          .replace(/^customer__/, ''),
    );
    await vscode.commands.executeCommand('escurel.resolve', `[[customer::${slug}]]`);
    const tab = await until(() => vscode.window.tabGroups.activeTabGroup.activeTab);
    assert.ok(tab.label.includes(slug), tab.label);
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });
});
