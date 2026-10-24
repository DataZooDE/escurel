import { afterEach, describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import { createContributions, createExtensionApi } from '../../src/api';
import { PageAsUiEditor } from '../../src/editors/pageAsUi';
import type { HostToWebview } from '../../src/shared/protocol';

const PAGE = 'markdown/instances/acme-thing/a.md';
const skillRow = {
  id: 'acme-thing',
  description: 'A thing.',
  required_frontmatter: [],
  optional_frontmatter: [],
  is_event_typed: false,
  visibility: 'public',
  owner_field: null,
  backend: { kind: 'markdown' },
  capabilities: { writable: true, granularity: 'block', search: 'hybrid', supports_crdt: false },
  layer: 'overlay',
  autonomy: 'review',
  fields: [],
};

function harness() {
  const changed = new vscode.EventEmitter<void>();
  const drafts: unknown[] = [];
  const client = {
    expand: async () => ({
      page: { skill: 'acme-thing', page_id: PAGE, slug: 'a', page_kind: 'instance' },
      frontmatter: { title: 'A', n: 1 },
      body: '',
    }),
    listSkills: async () => [skillRow],
    listEvents: async () => ({ events: [] }),
    createDraft: async (a: unknown) => (drafts.push(a), { ok: true, draft: { draft_id: 'd1' } }),
    callTool: async () => ({}),
  };
  const deps = {
    client: () => client as never,
    gatewayUrl: () => 'http://gw.test',
    onDidChange: changed.event,
  };
  const contributions = createContributions(deps);
  const posted: HostToWebview[] = [];
  let receive: (m: unknown) => void = () => undefined;
  const panel = {
    webview: {
      options: {},
      html: '',
      cspSource: 'x:',
      asWebviewUri: (u: unknown) => u,
      postMessage: (m: HostToWebview) => (posted.push(m), Promise.resolve(true)),
      onDidReceiveMessage: (f: (m: unknown) => void) => ((receive = f), { dispose() {} }),
    },
    title: '',
    onDidDispose: () => ({ dispose() {} }),
    onDidChangeViewState: () => ({ dispose() {} }),
  } as unknown as vscode.WebviewPanel;
  const editor = new PageAsUiEditor(
    { extensionUri: vscode.Uri.file('/ext/escurel'), subscriptions: [] } as never,
    () => client as never,
    changed.event,
    contributions,
  );
  const doc = { uri: vscode.Uri.file('/instances/acme-thing/a.md'), dispose() {} };
  const api = createExtensionApi(deps, contributions);
  const lastPage = async () => {
    await vi.waitFor(() => expect(posted.some((p) => p.type === 'page')).toBe(true));
    return [...posted].reverse().find((p) => p.type === 'page') as Extract<HostToWebview, { type: 'page' }>;
  };
  return { editor, doc, panel, api, posted, drafts, send: (m: unknown) => receive(m), lastPage };
}

afterEach(() => vi.restoreAllMocks());

describe('page augment wiring', () => {
  it('adds the augment to the page model of its skill and nothing to another skill', async () => {
    const h = harness();
    h.api.registerPageAugment('acme-thing', {
      sections: async (c) => [
        { id: 's', title: 'Extra', blocks: [{ type: 'text', text: `page ${c.pageId}` }] },
      ],
    });
    h.api.registerPageAugment('other', {
      sections: async () => [{ id: 'x', title: 'Other', blocks: [] }],
    });
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    h.send({ type: 'ready' });
    const page = await h.lastPage();
    expect(page.model.augment?.sections.map((s) => s.title)).toEqual(['Extra']);
  });

  it('a model without a registered augment has no augment member', async () => {
    const h = harness();
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    h.send({ type: 'ready' });
    expect((await h.lastPage()).model.augment).toBeUndefined();
  });

  it('runs the host command behind a button the host offered, and refuses an id it did not', async () => {
    const run = vi.spyOn(vscode.commands, 'executeCommand');
    const h = harness();
    h.api.registerPageAugment('acme-thing', {
      actions: () => [{ id: 'go', label: 'Go', command: 'acme.example.go', args: ['x'] }],
    });
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    h.send({ type: 'ready' });
    const page = await h.lastPage();
    const id = page.model.augment!.actions[0]!.id;
    h.send({ type: 'augment-action', id });
    h.send({ type: 'augment-action', id: 'a:999' });
    h.send({ type: 'augment-action', id: 'acme.example.go' });
    await vi.waitFor(() => expect(run).toHaveBeenCalledWith('acme.example.go', 'x'));
    expect(run.mock.calls.filter((c) => c[0] === 'acme.example.go')).toHaveLength(1);
  });

  it('a form submit calls create_draft with the validated draft only, and reports the result', async () => {
    const h = harness();
    h.api.registerPageAugment('acme-thing', {
      forms: [
        {
          id: 'note',
          title: 'Note',
          fields: [{ name: 'text', label: 'Text', kind: 'text', required: true }],
          toDraft: (v, c) => ({ target_page_id: c.pageId, content: `n: ${String(v.text)}` }),
        },
      ],
    });
    await h.editor.resolveCustomEditor(h.doc, h.panel);
    h.send({ type: 'ready' });
    const page = await h.lastPage();
    const id = page.model.augment!.forms[0]!.id;
    h.send({ type: 'augment-form', id, values: { text: '' } });
    await vi.waitFor(() =>
      expect(h.posted.some((p) => p.type === 'augment-result' && !p.ok)).toBe(true),
    );
    expect(h.drafts).toEqual([]);
    h.send({ type: 'augment-form', id, values: { text: 'hello' } });
    await vi.waitFor(() =>
      expect(h.posted.some((p) => p.type === 'augment-result' && p.ok)).toBe(true),
    );
    expect(h.drafts).toEqual([{ target_page_id: PAGE, content: 'n: hello' }]);
  });
});
