import * as vscode from 'vscode';
import type { AdminState } from '../auth/adminState';
import type { Event } from '../client';
import { EventSocket } from '../client/ws';
import { describeError } from '../errors';
import { latest } from '../shared/latest';
import { log } from '../log';
import type { Services } from '../services';
import {
  ESCUREL_RUNNER_STATUS_INTERVAL_MS,
  parseRunnerStatusBody,
  type RunnerStatusBody,
} from './runnerModel';
import {
  emptySnapshot,
  loadOlderRunEvents,
  readRunnerStatus,
  recordsFrom,
  refreshRunEvents,
  resolveSkills,
  type RunsSnapshot,
} from './runsLoader';
import { registerRunsCommands } from './runsCommands';
import {
  buildRunsTree,
  describeRunner,
  filterNote,
  groupRuns,
  stateWord,
  type RunRecord,
  type RunState,
  type RunsFilter,
  type RunsNode,
} from './runsModel';

/** The health sentence is re-derived from what is held (it fetches nothing); running rows tick. */
const REDRAW_MS = 2_000;
/** The runner's own heartbeat is the only thing that says it is still there: ask for it regularly. */
const STATUS_POLL_MS = 15_000;
const PAGE_OF_HISTORY = 25;

const ICONS: Record<RunState, [string, string | undefined]> = {
  running: ['sync~spin', 'charts.blue'],
  planned: ['checklist', 'charts.yellow'],
  succeeded: ['pass', 'testing.iconPassed'],
  failed: ['error', 'testing.iconFailed'],
  dead_letter: ['error', 'testing.iconFailed'],
  cancelled: ['circle-slash', undefined],
  unknown: ['question', undefined],
};

/**
 * The runs control center (secondary sidebar): what is running and can be cancelled, what waits for
 * you, what failed and can be retried, and the history of runs with their traces one click away.
 * The rows come from `runsModel`; this class only fetches, ticks and maps rows to tree items.
 */
export class RunnerTree implements vscode.TreeDataProvider<RunsNode>, vscode.Disposable {
  private readonly changed = new vscode.EventEmitter<RunsNode | undefined>();
  readonly onDidChangeTreeData = this.changed.event;

  private treeView?: vscode.TreeView<RunsNode>;
  private socket?: EventSocket;
  private focusSubscription?: vscode.Disposable;
  private snapshot: RunsSnapshot = emptySnapshot();
  private status: { at?: string | null; body?: RunnerStatusBody | null } | null = null;
  private intervalMs = ESCUREL_RUNNER_STATUS_INTERVAL_MS;
  private adminState: AdminState = 'unknown';
  private readonly skills = new Map<string, string>();
  private records: RunRecord[] = [];
  private roots: RunsNode[] = [];
  private filter: RunsFilter = {};
  private historyLimit = PAGE_OF_HISTORY;
  private loadError: string | undefined;
  private refreshTimer?: NodeJS.Timeout;
  private redrawTimer?: NodeJS.Timeout;
  private pollTimer?: NodeJS.Timeout;
  private readonly disposables: vscode.Disposable[] = [];
  private isDisposed = false;
  private isFetching = false;
  private pendingRefetch = false;
  private loaded = false;
  /** Retires a fetch that was started for a gateway or tenant the user has since left. */
  private readonly loads = latest();

  constructor(private readonly services: Services) {
    this.disposables.push(
      this.services.admin.onDidChange(async () => {
        this.adminState = await this.services.admin.get().catch(() => 'unknown' as const);
        this.rebuild();
      }),
      this.services.onDidChange(() => {
        // A different gateway or tenant: nothing held belongs to it, and a fetch in flight asked the old one.
        this.snapshot = emptySnapshot();
        this.status = null;
        this.skills.clear();
        this.records = [];
        this.historyLimit = PAGE_OF_HISTORY;
        this.loadError = undefined;
        this.loaded = false;
        this.loads.invalidate();
        this.rebuildSocket();
        void this.refresh();
      }),
    );
    this.rebuildSocket();
    this.redrawTimer = setInterval(() => {
      if (this.isDisposed || !this.loaded) return;
      this.rebuild();
    }, REDRAW_MS);
    this.pollTimer = setInterval(() => {
      if (!this.isDisposed && this.loaded) void this.refresh();
    }, STATUS_POLL_MS);
  }

  /** The sentence at the top of the view (runner health, active filter). Read by the integration suite. */
  get viewMessage(): string {
    return this.treeView?.message ?? '';
  }

  bindView(treeView: vscode.TreeView<RunsNode>): void {
    this.treeView = treeView;
    this.decorate();
  }

  // --- tree ---------------------------------------------------------------------------------

  getTreeItem(node: RunsNode): vscode.TreeItem {
    const collapsible = node.children?.length
      ? node.expanded
        ? vscode.TreeItemCollapsibleState.Expanded
        : vscode.TreeItemCollapsibleState.Collapsed
      : vscode.TreeItemCollapsibleState.None;
    const item = new vscode.TreeItem(node.label, collapsible);
    item.id = node.id;
    if (node.description) item.description = node.description;
    if (node.tooltip) item.tooltip = node.tooltip;
    if (node.contextValue) item.contextValue = node.contextValue;

    switch (node.kind) {
      case 'dispatch':
        item.iconPath = new vscode.ThemeIcon(
          node.contextValue === 'dispatch.paused' ? 'debug-pause' : 'debug-start',
          node.contextValue === 'dispatch.paused'
            ? new vscode.ThemeColor('charts.yellow')
            : undefined,
        );
        item.accessibilityInformation = {
          label: `${node.label}. ${node.description ?? ''}`,
          role: 'treeitem',
        };
        break;
      case 'insight':
        item.iconPath = new vscode.ThemeIcon('graph');
        break;
      case 'group':
        item.accessibilityInformation = {
          label: `${node.label}, ${node.description ?? '0'}`,
          role: 'treeitem',
        };
        break;
      case 'run': {
        const state = node.state ?? 'unknown';
        const [id, color] = ICONS[state];
        item.iconPath = new vscode.ThemeIcon(id, color ? new vscode.ThemeColor(color) : undefined);
        // The state is a WORD in the description and in the accessible name: never colour alone.
        item.accessibilityInformation = {
          label: `${stateWord(state)}: ${node.label}. ${node.description ?? ''}`,
          role: 'treeitem',
        };
        item.command = { command: 'escurel.openRun', title: 'Open run', arguments: [node] };
        break;
      }
      case 'reason':
        item.iconPath = new vscode.ThemeIcon(
          'debug-stackframe-dot',
          new vscode.ThemeColor('testing.iconFailed'),
        );
        item.accessibilityInformation = { label: `Reason: ${node.label}`, role: 'treeitem' };
        break;
      case 'more':
        item.iconPath = new vscode.ThemeIcon('chevron-down');
        item.command =
          node.id === 'more:attention'
            ? { command: 'escurel.runs.showFailed', title: 'Show failures in History' }
            : { command: 'escurel.runs.loadMore', title: 'Load more' };
        break;
      case 'error':
        item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'));
        item.command = { command: 'escurel.runs.refresh', title: 'Try again' };
        break;
      case 'empty':
        item.iconPath = new vscode.ThemeIcon('info');
        break;
    }
    return item;
  }

  async getChildren(node?: RunsNode): Promise<RunsNode[]> {
    if (node) return node.children ?? [];
    if (!this.loaded) await this.loadData();
    return this.roots;
  }

  // --- filter and paging (commands) ---------------------------------------------------------

  getFilter(): RunsFilter {
    return this.filter;
  }

  /** The skills seen in the loaded runs, for the filter's pick list. */
  knownSkills(): string[] {
    return [...new Set(this.records.map((r) => r.skill).filter((s): s is string => !!s))].sort();
  }

  async setFilter(filter: RunsFilter): Promise<void> {
    this.filter = filter;
    this.historyLimit = PAGE_OF_HISTORY;
    await vscode.commands.executeCommand(
      'setContext',
      'escurel.runs.filtered',
      !!(filter.states?.length || filter.skill || filter.text),
    );
    this.rebuild();
  }

  async loadMore(): Promise<void> {
    // First show what is already loaded; only when that is shown, ask the gateway for older events.
    const shown = this.historyLimit;
    const have = groupRuns(this.records, Date.now()).history.length;
    if (have > shown) {
      this.historyLimit = shown + PAGE_OF_HISTORY;
      this.rebuild();
      return;
    }
    const mine = this.loads.begin();
    try {
      const next = await loadOlderRunEvents(this.services.client, this.snapshot, PAGE_OF_HISTORY);
      if (!this.loads.isCurrent(mine)) return;
      this.snapshot = next;
      this.historyLimit = shown + PAGE_OF_HISTORY;
      await this.resolveAndRebuild(mine);
    } catch (err) {
      void vscode.window.showWarningMessage(`Couldn't load older runs: ${describeError(err)}`);
    }
  }

  async refresh(): Promise<void> {
    await this.loadData();
    this.changed.fire(undefined);
  }

  // --- data ---------------------------------------------------------------------------------

  private async loadData(): Promise<void> {
    if (this.isDisposed) return;
    if (this.isFetching) {
      this.pendingRefetch = true;
      return;
    }
    this.isFetching = true;
    const mine = this.loads.begin();
    try {
      const client = this.services.client;
      try {
        this.adminState = await this.services.admin.get();
      } catch (err) {
        log().debug(`runs view: error fetching admin state: ${describeError(err)}`);
      }
      if (!this.loads.isCurrent(mine)) return;

      let error: string | undefined;
      try {
        const [status, snapshot] = await Promise.all([
          readRunnerStatus(client),
          refreshRunEvents(client, this.snapshot),
        ]);
        if (!this.loads.isCurrent(mine)) return;
        this.status = status.event
          ? { at: status.event.at, body: parseRunnerStatusBody(status.event) }
          : null;
        this.intervalMs = status.intervalMs;
        this.snapshot = snapshot;
      } catch (err) {
        error = describeError(err);
        log().debug(`runs view: error loading runs: ${error}`);
      }
      this.loadError = error;
      this.loaded = true;
      await this.resolveAndRebuild(mine);
    } finally {
      this.isFetching = false;
      if (this.pendingRefetch) {
        this.pendingRefetch = false;
        await this.loadData();
      }
    }
  }

  /** Folds the events, looks up the skills the rows are missing, and redraws. */
  private async resolveAndRebuild(mine: number): Promise<void> {
    this.records = this.fold();
    this.rebuild();
    const missing = this.records
      .filter((r) => !r.skill && r.triggerEventId)
      .map((r) => r.triggerEventId!);
    if (missing.length === 0) return;
    try {
      const asked = await resolveSkills(this.services.client, missing, this.skills);
      if (asked && this.loads.isCurrent(mine) && !this.isDisposed) {
        this.records = this.fold();
        this.rebuild();
      }
    } catch (err) {
      log().debug(`runs view: error resolving skills: ${describeError(err)}`);
    }
  }

  private fold(): RunRecord[] {
    const live = this.status?.body?.live_runs;
    return recordsFrom(
      this.snapshot,
      Date.now(),
      live ? new Set(live.map((r) => r.run_id)) : undefined,
      this.skills,
    );
  }

  private rebuild(): void {
    if (this.isDisposed) return;
    const now = Date.now();
    this.records = this.fold();
    const runner = this.runnerDescription(now);
    this.roots = buildRunsTree({
      records: this.records,
      filter: this.filter,
      nowMs: now,
      historyLimit: this.historyLimit,
      hasMoreHistory: this.snapshot.hasMoreOlder,
      runner,
      isAdmin: this.adminState === 'admin',
      error: this.loadError,
    });
    void vscode.commands.executeCommand(
      'setContext',
      'escurel.runs.dispatchPaused',
      runner?.paused ?? false,
    );
    this.decorate();
    this.changed.fire(undefined);
  }

  private runnerDescription(now: number) {
    return describeRunner(this.status, now, {
      isAdmin: this.adminState === 'admin',
      tenant: this.status?.body?.tenant,
      intervalMs: this.intervalMs,
    });
  }

  /** The view's own header: the runner sentence, the filter note, the insight line, the attention badge. */
  private decorate(): void {
    const view = this.treeView;
    if (!view) return;
    const now = Date.now();
    const d = this.runnerDescription(now);
    const f = this.filter;
    const note = filterNote(f);
    // The insight line lives in the message: a view's description is not shown in this header.
    view.message = [d.text, note ? `Filtered: ${note}` : undefined].filter(Boolean).join(' · ');
    const g = groupRuns(this.records, now);
    const needs = g.waiting.length + g.attention.length;
    view.badge =
      needs > 0
        ? { value: needs, tooltip: `${needs} run${needs === 1 ? '' : 's'} need you` }
        : undefined;
  }

  // --- live ---------------------------------------------------------------------------------

  private scheduleRefresh(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => void this.refresh(), 300);
  }

  private rebuildSocket(): void {
    if (this.isDisposed) return;
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    this.focusSubscription = undefined;
    if (!this.services.gatewayUrl) return;

    // Run lifecycle events (`run-started`, `run-finished`, ...) carry the label `escurel:run`: a run
    // starting or ending reaches the panel as it happens. The runner's heartbeat is polled instead.
    this.socket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      filters: { label_skill: 'escurel:run' },
      onEvent: (_e: Event) => this.scheduleRefresh(),
      onConnect: () => void this.refresh(),
      onWarning: (kind, message) => {
        if (kind === 'session_cap_reached') {
          log().warn(`runs view: ${message}; falling back to refresh-on-focus`);
          this.socket?.close();
          this.focusSubscription ??= vscode.window.onDidChangeWindowState((s) => {
            if (s.focused) void this.refresh();
          });
          this.disposables.push(this.focusSubscription);
          return;
        }
        log().warn(`runs view: live warning: ${message}`);
        void this.refresh();
      },
      onError: (err) => log().warn(`runs view: live error: ${describeError(err)}`),
    });
    this.socket.connect();
  }

  dispose(): void {
    this.isDisposed = true;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    if (this.redrawTimer) clearInterval(this.redrawTimer);
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    for (const d of this.disposables) d.dispose();
    this.disposables.length = 0;
  }
}

/** Registers the runs tree view and the commands that work on its rows. */
export function registerRunnerView(
  context: vscode.ExtensionContext,
  services: Services,
  viewOrTree?: RunnerTree | vscode.TreeView<RunsNode>,
): RunnerTree {
  let tree: RunnerTree;
  let view: vscode.TreeView<RunsNode>;
  if (viewOrTree instanceof RunnerTree) {
    tree = viewOrTree;
    view = vscode.window.createTreeView('escurel.runner', {
      treeDataProvider: tree,
      showCollapseAll: false,
    });
    context.subscriptions.push(view);
  } else if (viewOrTree) {
    view = viewOrTree;
    tree = new RunnerTree(services);
    context.subscriptions.push(tree);
  } else {
    tree = new RunnerTree(services);
    view = vscode.window.createTreeView('escurel.runner', {
      treeDataProvider: tree,
      showCollapseAll: false,
    });
    context.subscriptions.push(view, tree);
  }
  tree.bindView(view);
  context.subscriptions.push(registerRunsCommands(tree));
  return tree;
}
