import { latest } from '../shared/latest';
import * as vscode from 'vscode';
import type { AdminState } from '../auth/adminState';
import type { Skill } from '../client/types';
import type { InspectorView, ThreadHostToWebview, ThreadWebviewToHost } from '../shared/protocol';
import { safePost } from '../shared/safePost';
import type { Services } from '../services';
import { describeError } from '../errors';
import { LiveViewSocket } from '../liveView';
import { log } from '../log';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import type { InspectorExtras } from './inspector';
import { buildNodeActions, resolveThreadAction } from './inspectorActions';
import { focusGraph, layoutThread } from './layout';
import { loadThread, type LoadedThread } from './loadThread';
import {
  collapsibleNodeId,
  commandForTarget,
  knownNodeId,
  resolveGate,
  rootEventIdOf,
} from './nodeTarget';
import { toThreadView } from './threadModel';

/** Events arrive in bursts when a run finishes; one refetch serves the whole burst. */
const REFETCH_DEBOUNCE_MS = 300;

/** Builds the inspector for every node. Injected so the controller does not own its content. */
export type DetailsBuilder = (
  loaded: LoadedThread,
  extras?: InspectorExtras,
) => Record<string, InspectorView>;

interface Open {
  panel: vscode.WebviewPanel;
  getCurrent: () => LoadedThread | undefined;
  getSkills: () => readonly Skill[] | undefined;
  getAdmin: () => AdminState;
  reload: () => void;
  render: () => void;
  select: (nodeId: string) => void;
  detailFor: (nodeId: string) => InspectorView | undefined;
  toggleCollapse: (nodeId: string) => void;
  expandAll: () => void;
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
    /** The canvas's collapsed cards, so the outline can show the same subtrees closed. */
    collapsed?: ReadonlySet<string>;
  }>();
  /** The outline follows whichever thread the user last looked at. */
  readonly onDidLoad = this.loaded.event;
  private readonly collapseChanged = new vscode.EventEmitter<{
    rootEventId: string;
    collapsed: ReadonlySet<string>;
  }>();
  /** A card was collapsed or expanded on the canvas (or "Expand all"). */
  readonly onDidCollapse = this.collapseChanged.event;
  private readonly selected = new vscode.EventEmitter<{ rootEventId: string; nodeId: string }>();
  readonly onDidSelect = this.selected.event;
  private readonly detailsChanged = new vscode.EventEmitter<{
    rootEventId: string;
    /** Absent: nothing selected any more (or the thread closed). */
    nodeId?: string;
    detail?: InspectorView;
    /** `select`: the user chose this node. `refresh`: the same node, after a live reload. */
    reason: 'select' | 'refresh';
  }>();
  /** What the details view should show: the selected node of a thread, with its inspector. */
  readonly onDidChangeDetails = this.detailsChanged.event;

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

  /**
   * Collapse or expand a card's subtree, exactly as a click on the canvas does (the webview's
   * message calls the same function). Public so a caller that holds the controller — the
   * integration suite — can do it without a click in a webview it cannot reach.
   */
  toggleCollapse(rootEventId: string, nodeId: string): void {
    this.panels.get(rootEventId)?.toggleCollapse(nodeId);
  }

  /** Whether a thread panel is open for this root event. */
  isOpen(rootEventId: string): boolean {
    return this.panels.has(rootEventId);
  }

  /** Whether this thread's panel is the active editor (it had the focus). */
  isActive(rootEventId: string): boolean {
    return this.panels.get(rootEventId)?.panel.active ?? false;
  }

  /** Give the keyboard focus back to a thread's canvas (Esc in the details view). */
  focusCanvas(rootEventId: string): void {
    const open = this.panels.get(rootEventId);
    if (open) open.panel.reveal(open.panel.viewColumn, false);
  }

  /** The toolbar's "Expand all". */
  expandAll(rootEventId: string): void {
    this.panels.get(rootEventId)?.expandAll();
  }

  /** Move the canvas's selection, from the outline. */
  select(rootEventId: string, nodeId: string): void {
    this.panels.get(rootEventId)?.select(nodeId);
  }

  /**
   * Dispatches a message arriving from a thread panel webview to the appropriate host command.
   * Host validates all arguments against the thread model and ignores forged parameters.
   * Public so integration tests can exercise host message handling directly.
   */
  async handleWebviewMessage(rootEventId: string, message: ThreadWebviewToHost): Promise<boolean> {
    const open = this.panels.get(rootEventId);
    if (!open) {
      log().warn(`thread: message for unknown panel ${rootEventId}`);
      return false;
    }
    const current = open.getCurrent();
    if (!current) {
      log().warn(`thread: message before thread loaded for ${rootEventId}`);
      return false;
    }

    const view = toThreadView(current);
    const resolved = resolveThreadAction(view, message, {
      admin: open.getAdmin(),
      skills: open.getSkills(),
      rawNodes: current.nodes,
      warn: (msg) => log().warn(msg),
    });

    if (!resolved) {
      return false;
    }

    await vscode.commands.executeCommand(resolved.command, ...resolved.args);
    return true;
  }

  open(arg: unknown): void {
    const rootEventId = rootEventIdOf(arg);
    if (!rootEventId) {
      void vscode.window.showInformationMessage('Pick an event to open its thread.');
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
    let cachedSkills: Skill[] | undefined;
    let cachedAdmin: AdminState = 'unknown';
    let timer: NodeJS.Timeout | undefined;
    let disposed = false;
    // The node the details view is showing for this thread, and the inspector built for every node
    // at the last render. The canvas no longer carries the inspector; the details view does.
    let selectedNodeId: string | undefined;
    let lastDetails: Record<string, InspectorView> = {};
    const fireDetails = (reason: 'select' | 'refresh') => {
      const detail = selectedNodeId ? lastDetails[selectedNodeId] : undefined;
      if (selectedNodeId && detail) {
        this.detailsChanged.fire({ rootEventId, nodeId: selectedNodeId, detail, reason });
      } else {
        selectedNodeId = undefined;
        this.detailsChanged.fire({ rootEventId, reason });
      }
    };

    const post = (m: ThreadHostToWebview) => {
      if (disposed) return;
      // The panel can be closed between the check above and the delivery; see `safePost`.
      safePost(panel, m);
    };
    // Layout is cheap and depends on `collapsed`, so collapsing never refetches.
    const render = () => {
      if (!current) return;
      const view = toThreadView(current);
      const layout = layoutThread(view, collapsed);
      const extras: InspectorExtras = {
        admin: cachedAdmin,
        skills: cachedSkills,
      };
      const rawNodes = [...current.nodes.values()];
      const details = this.details(current, extras);
      for (const node of view.nodes) {
        let d = details[node.id];
        if (!d && node.target.open === 'page') {
          d = {
            title: node.title,
            rows: [],
            sideTitle: '',
            side: [],
          };
          details[node.id] = d;
        }
        if (d && !d.actions) {
          const raw = current.nodes.get(node.id);
          const actions = buildNodeActions(node, raw, {
            admin: extras.admin,
            skills: extras.skills,
            rawById: current.nodes,
            lineageNodes: rawNodes,
          });
          if (actions) {
            d.actions = actions;
          }
        }
      }
      lastDetails = details;
      post({
        type: 'thread',
        view,
        layout,
        focus: focusGraph(view, layout),
      });
      // The node the user selected may have changed (a live reload) or gone.
      if (selectedNodeId) fireDetails('refresh');
      const root = view.nodes.find((n) => n.id === view.rootEventId);
      // The event's own title when it has one. `title` is the skill label, which every thread
      // from that skill shares: two open threads were both 'Thread · supplier-risk' and could
      // not be told apart in the tab bar.
      panel.title = `Thread · ${root?.subtitle || root?.title || rootEventId.slice(-6)}`;
    };
    const toggleCollapse = (nodeId: string) => {
      if (!collapsed.delete(nodeId)) collapsed.add(nodeId);
      render();
      this.collapseChanged.fire({ rootEventId, collapsed: new Set(collapsed) });
    };
    const expandAll = () => {
      collapsed.clear();
      render();
      this.collapseChanged.fire({ rootEventId, collapsed: new Set(collapsed) });
    };
    const loads = latest();
    const load = async () => {
      const mine = loads.begin();
      try {
        const [result, skills, admin] = await Promise.all([
          loadThread(this.services.client, rootEventId),
          this.services.client.listSkills().catch(() => undefined),
          this.services.admin.get().catch(() => 'unknown' as const),
        ]);
        // The panel can be closed while a read is in flight. A result arriving after that
        // belongs to nobody, and announcing it would update listeners (the outline) with a
        // thread that is no longer open — or with a stale one, if the panel was reopened.
        // A slower, earlier read must not overwrite a newer one.
        if (disposed || !loads.isCurrent(mine)) return;
        current = result;
        if (skills !== undefined) {
          cachedSkills = skills;
        }
        cachedAdmin = admin;
        render();
        this.loaded.fire({ rootEventId, thread: current, collapsed: new Set(collapsed) });
      } catch (err) {
        post({ type: 'thread-error', message: describeError(err), canReconnect: true });
      }
    };
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => void load(), REFETCH_DEBOUNCE_MS);
    };

    const adminSub = this.services.admin.onDidChange(async () => {
      const admin = await this.services.admin.get().catch(() => 'unknown' as const);
      if (disposed) return;
      cachedAdmin = admin;
      render();
    });

    // A different gateway or tenant: everything this panel holds (the thread, the cached skills and
    // admin state, the node the details view shows, the details built from it) belongs to the old one.
    // Retire the reads in flight and start over, so an action offered from the old state can never be
    // run against the new client.
    const switchSub = this.services.onDidChange(() => {
      loads.invalidate();
      clearTimeout(timer);
      current = undefined;
      cachedSkills = undefined;
      cachedAdmin = 'unknown';
      lastDetails = {};
      selectedNodeId = undefined;
      this.detailsChanged.fire({ rootEventId, reason: 'refresh' });
      post({ type: 'thread-loading', rootEventId });
      void load();
    });

    post({ type: 'thread-loading', rootEventId });
    const live = new LiveViewSocket(
      this.services,
      { root_event_id: rootEventId },
      () => schedule(),
      () => void load(),
    );

    const sub = panel.webview.onDidReceiveMessage((m: ThreadWebviewToHost) => {
      if (!m || typeof m !== 'object') return;
      switch (m.type) {
        case 'ready':
        case 'refresh':
          return void load();
        case 'select-node': {
          // Only a node of this thread: the outline reveals whatever it is told to.
          if (!knownNodeId(current && toThreadView(current), m.nodeId)) return;
          selectedNodeId = m.nodeId;
          fireDetails('select');
          return void this.selected.fire({ rootEventId, nodeId: m.nodeId });
        }
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
        case 'start-skill':
        case 'run-control':
        case 'view-skill':
          return void this.handleWebviewMessage(rootEventId, m);
        case 'toggle-collapse':
          if (!collapsibleNodeId(current && toThreadView(current), m.nodeId)) return;
          return toggleCollapse(m.nodeId);
        case 'expand-all':
          return expandAll();
      }
    });

    return {
      panel,
      getCurrent: () => current,
      getSkills: () => cachedSkills,
      getAdmin: () => cachedAdmin,
      reload: () => void load(),
      render: () => render(),
      select: (nodeId) => {
        post({ type: 'thread-select', nodeId });
        // The outline selected it: the details follow, exactly as for a click on the canvas.
        if (!knownNodeId(current && toThreadView(current), nodeId)) return;
        selectedNodeId = nodeId;
        fireDetails('select');
      },
      detailFor: (nodeId) => lastDetails[nodeId],
      toggleCollapse,
      expandAll,
      dispose: () => {
        disposed = true;
        this.detailsChanged.fire({ rootEventId, reason: 'refresh' });
        clearTimeout(timer);
        adminSub.dispose();
        switchSub.dispose();
        live.dispose();
        sub.dispose();
      },
    };
  }

  dispose(): void {
    for (const p of [...this.panels.values()]) p.panel.dispose();
    this.loaded.dispose();
    this.collapseChanged.dispose();
    this.selected.dispose();
    this.detailsChanged.dispose();
    log().info('escurel: thread panels closed');
  }
}
