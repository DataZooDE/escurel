import * as vscode from 'vscode';
import type { InspectorView, ThreadHostToWebview, ThreadWebviewToHost } from '../shared/protocol';
import type { Services } from '../services';
import { describeError } from '../errors';
import { LiveViewSocket } from '../liveView';
import { log } from '../log';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { focusGraph, layoutThread } from './layout';
import { loadThread, type LoadedThread } from './loadThread';
import { commandForTarget, resolveGate, rootEventIdOf } from './nodeTarget';
import { toThreadView } from './threadModel';

/** Events arrive in bursts when a run finishes; one refetch serves the whole burst. */
const REFETCH_DEBOUNCE_MS = 300;

/** Builds the inspector for every node. Injected so the controller does not own its content. */
export type DetailsBuilder = (loaded: LoadedThread) => Record<string, InspectorView>;

interface Open {
  panel: vscode.WebviewPanel;
  reload: () => void;
  select: (nodeId: string) => void;
  dispose: () => void;
}

/**
 * Thread webviews (SPEC §3.5): one panel per root event. Reads the lineage, folds and lays
 * it out, posts the result, and keeps it live over its own `root_event_id` subscription.
 *
 * The webview never sees a token or a tool name, and it never decides anything: a click is a
 * message, and the host routes it to the command that owns that surface.
 */
export class ThreadController implements vscode.Disposable {
  private readonly panels = new Map<string, Open>();
  private readonly loaded = new vscode.EventEmitter<{
    rootEventId: string;
    thread?: LoadedThread;
  }>();
  /** The outline follows whichever thread the user last looked at. */
  readonly onDidLoad = this.loaded.event;
  private readonly selected = new vscode.EventEmitter<{ rootEventId: string; nodeId: string }>();
  readonly onDidSelect = this.selected.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly services: Services,
    private readonly details: DetailsBuilder = () => ({}),
  ) {}

  static register(
    context: vscode.ExtensionContext,
    services: Services,
    details?: DetailsBuilder,
  ): ThreadController {
    const c = new ThreadController(context, services, details);
    context.subscriptions.push(
      c,
      vscode.commands.registerCommand('escurel.openThread', (arg?: unknown) => c.open(arg)),
    );
    return c;
  }

  /** Move the canvas's selection, from the outline. */
  select(rootEventId: string, nodeId: string): void {
    this.panels.get(rootEventId)?.select(nodeId);
  }

  open(arg: unknown): void {
    const rootEventId = rootEventIdOf(arg);
    if (!rootEventId) {
      void vscode.window.showInformationMessage('escurel: pick an event to open its thread.');
      return;
    }
    const existing = this.panels.get(rootEventId);
    if (existing) {
      existing.panel.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      'escurel.thread',
      'Thread',
      vscode.ViewColumn.Active,
      { ...webviewOptions(this.context.extensionUri), retainContextWhenHidden: true },
    );
    panel.webview.html = webviewHtml(
      panel.webview,
      this.context.extensionUri,
      'thread',
      'escurel-thread-canvas',
    );
    const open = this.wire(rootEventId, panel);
    this.panels.set(rootEventId, open);
    panel.onDidDispose(() => {
      open.dispose();
      this.panels.delete(rootEventId);
      this.loaded.fire({ rootEventId });
    });
  }

  private wire(rootEventId: string, panel: vscode.WebviewPanel): Open {
    const collapsed = new Set<string>();
    let current: LoadedThread | undefined;
    let timer: NodeJS.Timeout | undefined;
    let disposed = false;

    const post = (m: ThreadHostToWebview) => {
      if (!disposed) void panel.webview.postMessage(m);
    };
    // Layout is cheap and depends on `collapsed`, so collapsing never refetches.
    const render = () => {
      if (!current) return;
      const view = toThreadView(current);
      const layout = layoutThread(view, collapsed);
      post({
        type: 'thread',
        view,
        layout,
        focus: focusGraph(view, layout),
        details: this.details(current),
      });
      const root = view.nodes.find((n) => n.id === view.rootEventId);
      panel.title = `Thread · ${root?.title ?? rootEventId.slice(-6)}`;
    };
    const load = async () => {
      try {
        const result = await loadThread(this.services.client, rootEventId);
        // The panel can be closed while a read is in flight. A result arriving after that
        // belongs to nobody, and announcing it would update listeners (the outline) with a
        // thread that is no longer open — or with a stale one, if the panel was reopened.
        if (disposed) return;
        current = result;
        render();
        this.loaded.fire({ rootEventId, thread: current });
      } catch (err) {
        post({ type: 'thread-error', message: describeError(err), canReconnect: true });
      }
    };
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => void load(), REFETCH_DEBOUNCE_MS);
    };

    post({ type: 'thread-loading', rootEventId });
    const live = new LiveViewSocket(
      this.services,
      { root_event_id: rootEventId },
      () => schedule(),
      () => void load(),
    );

    const sub = panel.webview.onDidReceiveMessage((m: ThreadWebviewToHost) => {
      switch (m.type) {
        case 'ready':
        case 'refresh':
          return void load();
        case 'select-node':
          return void this.selected.fire({ rootEventId, nodeId: m.nodeId });
        case 'open-node': {
          const node = current && toThreadView(current).nodes.find((n) => n.id === m.nodeId);
          const cmd = node && commandForTarget(node.target);
          if (cmd) void vscode.commands.executeCommand(cmd.command, ...cmd.args);
          return;
        }
        case 'promote':
        case 'discard': {
          // Checked against the thread this host loaded, never trusted from the message: a
          // promote writes. The review commands own conflict and already-decided handling,
          // and the live event then refreshes this thread.
          const gate = current && resolveGate(toThreadView(current), m);
          if (!gate) {
            log().warn(`thread: refused a ${m.type} for ids this thread does not offer`);
            return;
          }
          return void vscode.commands.executeCommand(
            m.type === 'promote' ? 'escurel.promote' : 'escurel.discard',
            gate,
          );
        }
        case 'toggle-collapse':
          if (!collapsed.delete(m.nodeId)) collapsed.add(m.nodeId);
          return render();
        case 'expand-all':
          collapsed.clear();
          return render();
      }
    });

    return {
      panel,
      reload: () => void load(),
      select: (nodeId) => post({ type: 'thread-select', nodeId }),
      dispose: () => {
        disposed = true;
        clearTimeout(timer);
        live.dispose();
        sub.dispose();
      },
    };
  }

  dispose(): void {
    for (const p of [...this.panels.values()]) p.panel.dispose();
    this.loaded.dispose();
    this.selected.dispose();
    log().info('escurel: thread panels closed');
  }
}
