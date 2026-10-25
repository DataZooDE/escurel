import { afterEach, describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import type { Draft } from '../../src/client';
import { ReviewHookRegistry } from '../../src/review/hooks';
import { ReviewPreviewPanel } from '../../src/review/previewPanel';

const draft = { draft_id: 'd1', target_page_id: 'markdown/instances/acme-thing/a.md' } as Draft;
const gateway = { callTool: () => Promise.reject(new Error('unused')) };

function fakePanel() {
  const posted: unknown[] = [];
  let receive: (m: unknown) => void = () => undefined;
  let onDispose: () => void = () => undefined;
  const panel = {
    webview: {
      options: {},
      html: '',
      cspSource: 'x:',
      asWebviewUri: (u: unknown) => u,
      postMessage: (m: unknown) => (posted.push(m), Promise.resolve(true)),
      onDidReceiveMessage: (f: (m: unknown) => void) => ((receive = f), { dispose() {} }),
    },
    title: '',
    reveal() {},
    dispose: () => onDispose(),
    onDidDispose: (f: () => void) => ((onDispose = f), { dispose() {} }),
  };
  return { panel, posted, send: (m: unknown) => receive(m), disposed: () => onDispose };
}

function setup() {
  const hooks = new ReviewHookRegistry();
  const created: ReturnType<typeof fakePanel>[] = [];
  const preview = new ReviewPreviewPanel({
    hooks,
    gateway,
    extensionUri: vscode.Uri.file('/ext/escurel'),
    createPanel: () => {
      const f = fakePanel();
      created.push(f);
      return f.panel as never;
    },
  });
  return { hooks, preview, created };
}

afterEach(() => vi.restoreAllMocks());

describe('ReviewPreviewPanel', () => {
  it('opens nothing when no hook adds a section', async () => {
    const { hooks, preview, created } = setup();
    hooks.register({ before: () => undefined });
    await preview.show(draft);
    hooks.register({ preview: () => [] });
    await preview.show(draft);
    expect(created).toHaveLength(0);
  });

  it('opens one panel with the sections, loads the core bundle only, and reuses it for the next draft', async () => {
    const { hooks, preview, created } = setup();
    hooks.register({
      preview: (c) => [{ id: 's', title: `For ${c.draftId}`, blocks: [{ type: 'text', text: 't' }] }],
    });
    await preview.show(draft);
    await preview.show({ ...draft, draft_id: 'd2' });
    expect(created).toHaveLength(1);
    const { panel, posted } = created[0]!;
    expect(panel.webview.html).toContain('review-preview.js');
    expect(panel.webview.html).toContain('<escurel-review-preview');
    expect(panel.webview.html).not.toMatch(/#[0-9a-fA-F]{3,8}\b(?![^<]*<\/style)/);
    const views = posted.filter((m) => (m as { type: string }).type === 'review-preview') as {
      view: { sections: { title: string }[] };
    }[];
    expect(views.map((v) => v.view.sections[0]!.title)).toEqual(['For d1', 'For d2']);
  });

  it('runs the command behind a list line it offered and refuses any other handle', async () => {
    const run = vi.spyOn(vscode.commands, 'executeCommand');
    const { hooks, preview, created } = setup();
    hooks.register({
      preview: () => [
        {
          id: 's',
          title: 'S',
          blocks: [{ type: 'list', items: [{ label: 'open', command: 'acme.example.open', args: ['x'] }] }],
        },
      ],
    });
    await preview.show(draft);
    const { posted, send } = created[0]!;
    const view = (posted.find((m) => (m as { type: string }).type === 'review-preview') as {
      view: { sections: { blocks: { items: { key: string }[] }[] }[] };
    }).view;
    const key = view.sections[0]!.blocks[0]!.items[0]!.key;
    send({ type: 'augment-action', id: key });
    send({ type: 'augment-action', id: 'p:99' });
    send({ type: 'augment-action', id: 'acme.example.open' });
    send({ type: 'augment-form', id: 'x', values: {} });
    expect(run.mock.calls.filter((c) => c[0] === 'acme.example.open')).toEqual([
      ['acme.example.open', 'x'],
    ]);
  });

  it('closes the panel when the next draft has no preview, and starts again after the panel was closed', async () => {
    const { hooks, preview, created } = setup();
    hooks.register({
      preview: (c) =>
        c.draftId === 'd1' ? [{ id: 's', title: 'S', blocks: [{ type: 'text', text: 't' }] }] : [],
    });
    await preview.show(draft);
    await preview.show({ ...draft, draft_id: 'd2' });
    created[0]!.disposed()();
    await preview.show(draft);
    expect(created).toHaveLength(2);
  });

  it('answers a ready from the webview with what it shows', async () => {
    const { hooks, preview, created } = setup();
    hooks.register({ preview: () => [{ id: 's', title: 'S', blocks: [] }] });
    await preview.show(draft);
    const { posted, send } = created[0]!;
    const before = posted.length;
    send({ type: 'ready' });
    expect(posted.length).toBe(before + 1);
  });
});
