import { describe, expect, it } from 'vitest';
import * as vscode from 'vscode';
import { createContributions } from '../../src/api';
import { PageAsUiEditor } from '../../src/editors/pageAsUi';

// A companion's renderer takes over the panel for its skill; a throwing renderer, an unknown skill and a
// window without renderers all leave the generic page view as it was.
function harness(skill: string) {
  const changed = new vscode.EventEmitter<void>();
  let expands = 0;
  const client = {
    expand: async () => {
      expands += 1;
      return {
        page: { skill, page_id: 'markdown/instances/acme-thing/a.md' },
        frontmatter: { n: 1 },
      };
    },
    listSkills: () => new Promise(() => undefined), // keeps the generic path from finishing
    callTool: async () => ({}),
  };
  const contributions = createContributions({
    client: () => client as never,
    gatewayUrl: () => 'http://gw.test',
    onDidChange: changed.event,
  });
  const disposed: (() => void)[] = [];
  const posted: unknown[] = [];
  const panel = {
    webview: {
      options: {},
      html: '',
      cspSource: 'x:',
      asWebviewUri: (u: unknown) => u,
      postMessage: (m: unknown) => (posted.push(m), Promise.resolve(true)),
      onDidReceiveMessage: () => ({ dispose() {} }),
    },
    title: '',
    onDidDispose: (f: () => void) => (disposed.push(f), { dispose() {} }),
    onDidChangeViewState: () => ({ dispose() {} }),
  } as unknown as vscode.WebviewPanel;
  const editor = new PageAsUiEditor(
    { extensionUri: vscode.Uri.file('/ext/escurel'), subscriptions: [] } as never,
    () => client as never,
    changed.event,
    contributions,
  );
  const doc = { uri: vscode.Uri.file('/instances/acme-thing/a.md'), dispose() {} };
  return { editor, doc, panel, contributions, expands: () => expands, disposed, posted };
}

describe('page renderer delegation', () => {
  it('hands the panel to the renderer registered for the page skill', async () => {
    const h = harness('acme-thing');
    const seen: Record<string, unknown>[] = [];
    let cleaned = 0;
    h.contributions.renderers.register('acme-thing', {
      render: (ctx) => {
        seen.push({ pageId: ctx.pageId, skill: ctx.skill, fm: ctx.frontmatter });
        ctx.panel.webview.html = '<p>companion</p>';
        return { dispose: () => void (cleaned += 1) };
      },
    });
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    expect(seen).toEqual([
      { pageId: 'markdown/instances/acme-thing/a.md', skill: 'acme-thing', fm: { n: 1 } },
    ]);
    expect(h.panel.webview.html).toBe('<p>companion</p>');
    h.disposed.forEach((f) => f());
    expect(cleaned).toBe(1);
  });

  it('falls back to the generic view when the renderer throws', async () => {
    const h = harness('acme-thing');
    h.contributions.renderers.register('acme-thing', {
      render: () => {
        throw new Error('boom');
      },
    });
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    expect(h.panel.webview.html).toContain('<escurel-page-as-ui');
  });

  it('shows the generic view for a skill without a renderer, and reads nothing when none is registered', async () => {
    const other = harness('other-skill');
    other.contributions.renderers.register('acme-thing', { render: () => undefined });
    await other.editor.resolveCustomEditor(other.doc, other.panel);
    expect(other.panel.webview.html).toContain('<escurel-page-as-ui');

    const none = harness('acme-thing');
    await none.editor.resolveCustomEditor(none.doc, none.panel);
    expect(none.expands()).toBe(0);
  });
});
