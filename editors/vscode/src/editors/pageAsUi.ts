import { latestWriteBack, type WriteBackStatus } from '../shared/writeBack';
import { resolvePageMessage } from './pageMessages';
import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { pageIdFromPath } from '../fs/read';
import { log } from '../log';
import { buildPageModel } from '../shared/page';
import { latestLoader } from '../shared/latestLoader';
import { newNonce } from './nonce';
import { safePost } from '../shared/safePost';
import { findThreadStrip } from '../shared/threadStrip';
import { pageTabTitle } from '../shared/tabTitle';
import type { Contributions } from '../api';
import { loadAugment, resolveAugmentAction, resolveAugmentSubmit, type AugmentTable } from '../api/augment';
import { parseViewer } from '../shared/viewer';
import { loadReport } from './reportLoader';
import type { HostToWebview, PageModel, WebviewToHost } from '../shared/protocol';

export const VIEW_TYPE = 'escurel.pageAsUi';

type PageContributions = Pick<Contributions, 'renderers' | 'gateway' | 'onDidChangeTenant'> &
  Partial<Pick<Contributions, 'augments'>>;

/** What the host knows of the companions' additions to the page it last sent. */
interface AugmentState {
  table?: AugmentTable;
  frontmatter: Record<string, unknown>;
  skill?: string;
}

/** Name the tab; a panel that was closed while the page loaded is not worth an error. */
function setTitle(panel: vscode.WebviewPanel, parts: Parameters<typeof pageTabTitle>[0]): void {
  try {
    panel.title = pageTabTitle(parts);
  } catch {
    // disposed
  }
}

/**
 * Page as UI (SPEC §3.4) as a CustomReadonlyEditor on `escurel:` instance
 * URIs: the host reads `expand` + the skill row, folds them into the
 * PageModel and posts it; the webview never sees a token or a tool name.
 */
export class PageAsUiEditor implements vscode.CustomReadonlyEditorProvider {
  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly client: () => EscurelClient,
    private readonly onDidChange: vscode.Event<void>,
    private readonly contributions?: PageContributions,
  ) {}

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
    onDidChange: vscode.Event<void>,
    contributions?: PageContributions,
  ): void {
    context.subscriptions.push(
      vscode.window.registerCustomEditorProvider(
        VIEW_TYPE,
        new PageAsUiEditor(context, client, onDidChange, contributions),
        {
          webviewOptions: { retainContextWhenHidden: true },
          supportsMultipleEditorsPerDocument: true,
        },
      ),
    );
  }

  openCustomDocument(uri: vscode.Uri): vscode.CustomDocument {
    return { uri, dispose: () => undefined };
  }

  async resolveCustomEditor(doc: vscode.CustomDocument, panel: vscode.WebviewPanel): Promise<void> {
    const pageId = pageIdFromPath(doc.uri.path)?.pageId;
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview')],
    };
    if (pageId && (await this.delegate(pageId, panel))) return;
    panel.webview.html = this.html(panel.webview);
    const post = (m: HostToWebview) => safePost(panel, m);
    // The model the host last built: a webview message is judged against THIS, never against what
    // the webview claims.
    let current: PageModel | undefined;
    // The loading state is for the FIRST load only. A reload caused by something live (every event
    // reloads the page) replaces the content quietly: flashing it to "loading" each time swallowed
    // clicks and reset the reader's place.
    let shown = false;
    // Reads overlap (every event, view-state change and gateway change starts one, each several awaits):
    // only the newest is applied, so a slow old read cannot replace the model the host validates against.
    const augmentState: AugmentState = { frontmatter: {} };
    const loader = latestLoader<{
      message: HostToWebview;
      model?: PageModel;
      augment?: AugmentState;
    }>(
      async () => {
        if (!pageId)
          return {
            message: { type: 'error', message: `not an escurel page: ${doc.uri.toString()}` },
          };
        const c = this.client();
        const [e, skills] = await Promise.all([c.expand({ page_id: pageId }), c.listSkills()]);
        if (!e.page)
          return {
            message: {
              type: 'error',
              message: 'This page does not exist, or you may not read it.',
            },
          };
        const skill = skills.find((s) => s.id === e.page!.skill);
        if (!skill)
          return {
            message: { type: 'error', message: `skill ${e.page.skill} is not in the catalogue` },
          };
        const fromPage = buildPageModel(e, skill);
        // The report that draws this skill's records is declared on the skill PAGE, not in the
        // catalogue row; a failed read just means no chart line.
        const skillPage = await c
          .expand({ page_id: `markdown/skills/${skill.id}.md` })
          .catch(() => undefined);
        const viewer = parseViewer(skillPage?.frontmatter);
        // The figures that report draws for THIS record (KPIs and tables); none when it cannot be shown.
        const report = viewer ? await loadReport(c, viewer.report, e.frontmatter ?? {}) : undefined;
        // Page chat is declared on the skill page too (`chat: {page_bound: true}`).
        const pageChat =
          (skillPage?.frontmatter as { chat?: { page_bound?: unknown } } | undefined)?.chat
            ?.page_bound === true;
        const withChat = pageChat ? { ...fromPage, chat: true as const } : fromPage;
        const withViewer = viewer ? { ...withChat, viewer } : withChat;
        const model = report ? { ...withViewer, report } : withViewer;
        // The tab is named after the URI's last segment (`all.md`) unless the host says otherwise.
        setTitle(panel, {
          title: model.title,
          skill: skill.id,
          slug: e.page.slug ?? e.page.page_id.split('/').pop()?.replace(/\.md$/, '') ?? '',
        });
        // Where the page came from. A failure here must not cost the user the page: the strip
        // is an addition to it, so it degrades to absent.
        const strip = await findThreadStrip((cursor) =>
          c.listEvents({
            instance_page_id: pageId,
            include_system: true,
            newest_first: true,
            limit: 50,
            ...(cursor ? { cursor } : {}),
          }),
        );
        // What the last change sent to the source did. Like the thread strip it is an addition: a
        // failure here must not cost the user the page.
        let writeBack: WriteBackStatus | undefined;
        // A row that can be written back to: REST/MCP, or a SQL database with writable columns.
        if (model.source?.external || model.source?.writableColumns?.length) {
          try {
            const evs = await c.listEvents({
              instance_page_id: pageId,
              label_skill: 'escurel:write-back',
              include_system: true,
              newest_first: true,
              limit: 20,
            });
            writeBack = latestWriteBack(evs.events);
          } catch {
            writeBack = undefined;
          }
        }
        const base = strip ? { ...model, thread: strip } : model;
        const withWriteBack = writeBack ? { ...base, writeBack } : base;
        // What companions add is read last and is an addition like the strip: a failure costs the
        // additions, never the page.
        const loaded = this.contributions?.augments?.has(skill.id)
          ? await loadAugment(this.contributions.augments, skill.id, {
              pageId,
              skill: skill.id,
              frontmatter: e.frontmatter ?? {},
              client: this.contributions.gateway,
            }).catch(() => undefined)
          : undefined;
        const built = loaded ? { ...withWriteBack, augment: loaded.view } : withWriteBack;
        return {
          message: { type: 'page', model: built },
          model: built,
          augment: { table: loaded?.table, frontmatter: e.frontmatter ?? {}, skill: skill.id },
        };
      },
      (result) => {
        if (result.model) {
          current = result.model;
          Object.assign(augmentState, result.augment);
          shown = true;
        }
        post(result.message);
      },
      (err) => post({ type: 'error', message: describeError(err) }),
    );
    const load = async () => {
      if (!shown) post({ type: 'loading' });
      await loader.run();
    };
    const subs: vscode.Disposable[] = [
      panel.webview.onDidReceiveMessage((m: WebviewToHost) => {
        if (m?.type === 'augment-action' || m?.type === 'augment-form') {
          return void this.onAugmentMessage(m, pageId, augmentState, post, load);
        }
        this.onMessage(m, pageId, current, load);
      }),
      // A different gateway or tenant: what is on screen (and what messages are judged against) belongs
      // to the old one. Retire the reads in flight and start over.
      this.onDidChange(() => {
        loader.invalidate();
        current = undefined;
        shown = false;
        void load();
      }),
      panel.onDidChangeViewState((e) => {
        if (e.webviewPanel.active) void load();
      }),
    ];
    panel.onDidDispose(() => {
      loader.invalidate();
      subs.forEach((s) => s.dispose());
    });
  }

  /**
   * A companion extension that registered a renderer for this page's skill draws the page itself. The
   * skill is read only when a renderer exists, so a window without companions pays nothing. A renderer
   * that throws leaves the generic view in place.
   */
  private async delegate(pageId: string, panel: vscode.WebviewPanel): Promise<boolean> {
    const c = this.contributions;
    if (!c || c.renderers.size === 0) return false;
    try {
      const e = await this.client().expand({ page_id: pageId });
      const renderer = e.page ? c.renderers.get(e.page.skill) : undefined;
      if (!e.page || !renderer) return false;
      const taken = await renderer.render({
        pageId,
        skill: e.page.skill,
        frontmatter: e.frontmatter ?? {},
        panel,
        client: c.gateway,
        onDidChangeTenant: c.onDidChangeTenant,
      });
      if (taken) panel.onDidDispose(() => taken.dispose());
      return true;
    } catch (err) {
      log().warn(`escurel: page renderer failed, showing the generic page: ${describeError(err)}`);
      return false;
    }
  }

  /**
   * A button, list line or form a companion added. The webview names a handle; the command, or the
   * draft a form may create, comes from the table the host built for the page on screen.
   */
  private async onAugmentMessage(
    m: Extract<WebviewToHost, { type: 'augment-action' | 'augment-form' }>,
    pageId: string | undefined,
    state: AugmentState,
    post: (m: HostToWebview) => void,
    load: () => Promise<void>,
  ): Promise<void> {
    const table = state.table;
    if (!table || !pageId || !state.skill) return;
    if (m.type === 'augment-action') {
      const cmd = resolveAugmentAction(table, m.id);
      if (!cmd) {
        log().warn('escurel: refused an augment action it did not offer');
        return;
      }
      void vscode.commands.executeCommand(cmd.command, ...cmd.args);
      return;
    }
    const result = (ok: boolean, message: string) =>
      post({ type: 'augment-result', id: String(m.id), ok, message });
    const submit = await resolveAugmentSubmit(table, m.id, m.values, {
      pageId,
      skill: state.skill,
      frontmatter: state.frontmatter,
      client: this.contributions!.gateway,
    });
    if (!submit.ok) return result(false, submit.message);
    try {
      await this.client().createDraft(submit.args);
      result(true, 'Proposed. A reviewer decides it in Awaiting you.');
      void load();
    } catch (err) {
      result(false, describeError(err));
    }
  }

  private onMessage(
    m: WebviewToHost,
    pageId: string | undefined,
    model: PageModel | undefined,
    load: () => Promise<void>,
  ): void {
    switch (m.type) {
      case 'ready':
      case 'refresh':
        return void load();
      case 'show-raw':
        return void vscode.commands.executeCommand('escurel.showRaw', pageId);
      default: {
        // Everything else is decided from the host's model, not from what the webview says.
        const resolved = resolvePageMessage(model, m);
        if (!resolved) {
          log().warn(
            `escurel: refused a page message of type ${String((m as { type?: unknown }).type)}`,
          );
          return;
        }
        if (m.type === 'start-skill')
          log().info(`escurel: start ${m.skill} (${m.mode}) on ${pageId}`);
        return void vscode.commands.executeCommand(resolved.command, ...resolved.args);
      }
    }
  }

  private html(webview: vscode.Webview): string {
    const script = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview', 'page-as-ui.js'),
    );
    const nonce = newNonce();
    return `<!doctype html><html><head><meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>body{margin:0}</style></head>
<body><escurel-page-as-ui></escurel-page-as-ui><script type="module" nonce="${nonce}" src="${script}"></script></body></html>`;
  }
}
