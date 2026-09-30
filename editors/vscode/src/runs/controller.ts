import * as vscode from 'vscode';
import type { RunHostToWebview, RunView, RunWebviewToHost } from '../shared/protocol';
import type { Services } from '../services';
import { describeError } from '../errors';
import { LiveViewSocket } from '../liveView';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { carryCalls, loadRun } from './loadRun';
import { mergeToolCallPage } from './runModel';

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
      void vscode.window.showInformationMessage('escurel: pick a run to open its detail.');
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

  private wire(runId: string, panel: vscode.WebviewPanel): void {
    let view: RunView | undefined;
    let rootEventId: string | undefined;
    let timer: NodeJS.Timeout | undefined;
    let disposed = false;

    const post = (m: RunHostToWebview) => {
      if (!disposed) void panel.webview.postMessage(m);
    };
    const load = async () => {
      try {
        const loaded = await loadRun(this.services.client, runId);
        view = carryCalls(loaded.view, view);
        rootEventId = loaded.rootEventId;
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

    const sub = panel.webview.onDidReceiveMessage(async (m: RunWebviewToHost) => {
      switch (m.type) {
        case 'ready':
        case 'refresh':
          return void load();
        case 'load-more-calls': {
          if (!view) return;
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
        case 'copy-trace-id':
          // The host owns the clipboard, so only the host can say it worked.
          await vscode.env.clipboard.writeText(m.traceId);
          void vscode.window.showInformationMessage('escurel: trace id copied.');
          return;
      }
    });

    const dispose = () => {
      disposed = true;
      clearTimeout(timer);
      live.dispose();
      sub.dispose();
      this.panels.delete(runId);
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
