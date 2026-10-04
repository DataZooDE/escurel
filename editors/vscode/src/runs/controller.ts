import { quietly } from '../shared/quiet';
import { runTabTitle } from './runTitle';
import * as vscode from 'vscode';
import type { RunHostToWebview, RunView, RunWebviewToHost } from '../shared/protocol';
import { safePost } from '../shared/safePost';
import type { Services } from '../services';
import { describeError } from '../errors';
import { LiveViewSocket } from '../liveView';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { carryCalls, loadRun } from './loadRun';
import { mergeToolCallPage } from './runModel';
import { runControls } from './controls';
import {
  acceptLoadMore,
  resolveRunAction,
  traceIdToCopy,
  visibleRunControls,
  type ActionRunView,
} from './runActions';
import { log } from '../log';

const REFETCH_DEBOUNCE_MS = 300;

/** The run id an argument names: a bare id, or a thread node's run target. */
export function runIdOf(arg: unknown): string | undefined {
  if (typeof arg === 'string') return arg || undefined;
  if (arg && typeof arg === 'object') {
    const o = arg as { runId?: unknown; id?: unknown };
    if (typeof o.runId === 'string' && o.runId) return o.runId;
    if (typeof o.id === 'string' && o.id) return o.id;
  }
  return undefined;
}

/**
 * Run detail (SPEC §3.6): one panel per run, live over its own `run_id` subscription. The
 * first page of tool calls is read with the run; "Load more" reads the next by `after`.
 */
export class RunController implements vscode.Disposable {
  private readonly panels = new Map<string, vscode.WebviewPanel>();
  private readonly views = new Map<string, ActionRunView>();
  private readonly disposers: (() => void)[] = [];
  private readonly loaded = new vscode.EventEmitter<{ runId: string; view: RunView }>();
  /** What the host last loaded for a run — the only view of it outside the webview. */
  readonly onDidLoad = this.loaded.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly services: Services,
  ) {}

  static register(context: vscode.ExtensionContext, services: Services): RunController {
    const c = new RunController(context, services);
    context.subscriptions.push(
      c,
      vscode.commands.registerCommand('escurel.openRun', (arg?: unknown) => c.open(arg)),
    );
    return c;
  }

  open(arg: unknown): void {
    const runId = runIdOf(arg);
    if (!runId) {
      void vscode.window.showInformationMessage('Select a run in the Runner view to open it.');
      return;
    }
    const existing = this.panels.get(runId);
    if (existing) {
      existing.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      'escurel.run',
      `Run · ${runId.slice(-6)}`,
      vscode.ViewColumn.Active,
      { ...webviewOptions(this.context.extensionUri), retainContextWhenHidden: true },
    );
    panel.webview.html = webviewHtml(
      panel.webview,
      this.context.extensionUri,
      'run',
      'escurel-run-detail',
    );
    this.panels.set(runId, panel);
    this.wire(runId, panel);
  }

  /** The panel and integration tests use this same guarded host path. */
  async handleWebviewMessage(runId: string, message: unknown): Promise<void> {
    const view = this.views.get(runId);
    const action = view && resolveRunAction(view, message);
    if (!action) {
      log().warn('run detail: rejected invalid action');
      return;
    }
    await vscode.commands.executeCommand(action.command, action.args);
  }

  private wire(runId: string, panel: vscode.WebviewPanel): void {
    let view: RunView | undefined;
    let rootEventId: string | undefined;
    let timer: NodeJS.Timeout | undefined;
    let disposed = false;

    const post = (m: RunHostToWebview) => {
      if (disposed) return;
      // The panel can be closed between the check above and the delivery; see `safePost`.
      safePost(panel, m);
    };
    let loadSeq = 0;
    const load = async () => {
      // Reads overlap (a reconnect, a live event, a manual refresh). Only the NEWEST one to start is
      // allowed to land: a slower earlier read finishing last must not overwrite a newer state.
      const mine = ++loadSeq;
      try {
        const loaded = await loadRun(this.services.client, runId);
        // See ThreadController: a result for a panel that has been closed is for nobody.
        if (disposed || mine !== loadSeq) return;
        const next = carryCalls(loaded.view, view) as ActionRunView;
        next.controls = visibleRunControls({
          ...next,
          controls: runControls(next.status, await this.services.admin.get()),
        });
        if (disposed || mine !== loadSeq) return;
        view = next;
        this.views.set(runId, next);
        rootEventId = loaded.rootEventId;
        // The tab names the run by skill and page once the load knows them.
        panel.title = runTabTitle(next);
        post({ type: 'run', view });
        this.loaded.fire({ runId, view });
      } catch (err) {
        post({ type: 'run-error', message: describeError(err), canReconnect: true });
      }
    };
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => void load(), REFETCH_DEBOUNCE_MS);
    };

    post({ type: 'run-loading', runId });
    const live = new LiveViewSocket(this.services, { run_id: runId }, schedule, () => void load());
    const adminSub = this.services.admin.onDidChange(schedule);

    const sub = panel.webview.onDidReceiveMessage(async (m: RunWebviewToHost) => {
      switch (m.type) {
        case 'ready':
        case 'refresh':
          return void load();
        case 'load-more-calls': {
          // Only the cursor the host offered with the last page; anything else is a forgery.
          if (!view || !acceptLoadMore(view, m.after)) return;
          try {
            const page = await this.services.client.getRunToolCalls({
              run_id: runId,
              limit: 50,
              after: m.after,
            });
            view = mergeToolCallPage(view, page);
            post({ type: 'run', view });
          } catch (err) {
            post({ type: 'run-error', message: describeError(err), canReconnect: true });
          }
          return;
        }
        case 'open-page':
          return void vscode.commands.executeCommand('escurel.openPage', m.pageId);
        case 'open-thread':
          return void vscode.commands.executeCommand(
            'escurel.openThread',
            m.rootEventId || rootEventId,
          );
        case 'copy-trace-id': {
          // The host owns the clipboard, so only the host can say it worked. It copies ITS trace
          // id, never the string the webview sent: a forged message must not be able to plant text
          // on the user's clipboard.
          const traceId = traceIdToCopy(view);
          if (!traceId) return;
          await vscode.env.clipboard.writeText(traceId);
          quietly('Trace id copied');
          return;
        }
        case 'copy-run-id': {
          // The panel's own run id (the host holds it): nothing the webview sent is copied.
          await vscode.env.clipboard.writeText(runId);
          quietly('Run id copied');
          return;
        }
        case 'run-control':
        case 'view-skill':
          return void this.handleWebviewMessage(runId, m);
      }
    });

    const dispose = () => {
      disposed = true;
      clearTimeout(timer);
      live.dispose();
      adminSub.dispose();
      sub.dispose();
      this.panels.delete(runId);
      this.views.delete(runId);
    };
    this.disposers.push(dispose);
    panel.onDidDispose(dispose);
  }

  dispose(): void {
    for (const p of [...this.panels.values()]) p.dispose();
    this.disposers.length = 0;
    this.loaded.dispose();
  }
}
