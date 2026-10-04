import { latest } from '../shared/latest';
import * as vscode from 'vscode';
import type { AdminState } from '../auth/adminState';
import type { EscurelClient, Event, EventsPage } from '../client';
import { EventSocket } from '../client/ws';
import { describeError } from '../errors';
import { log } from '../log';
import type { Services } from '../services';
import {
  ESCUREL_RUNNER_STATUS_INTERVAL_MS,
  buildRunnerRows,
  estimateHeartbeatIntervalMs,
  extractDeadLetters,
  parseRunnerStatusBody,
  type DeadLetterItem,
  type RunnerRow,
  type RunnerStatusBody,
} from './runnerModel';

/** How often the health row is re-derived from what is held (it fetches nothing). */
const HEALTH_REDRAW_MS = 5_000;

/**
 * Tree view for the Escurel Runner (SPEC §3.3, §3.9).
 * Placed in the secondary sidebar (on the right).
 *
 * Shows:
 * - Health (derived from heartbeat age and status)
 * - Runner identification & version
 * - Runs breakdown (live, processed, failed, dead letters, etc.)
 * - Live runs with drill-down to run detail
 * - Dead letters (newest 20) with drill-down to run detail
 * - Paused tenants and permit availability
 */
export class RunnerTree implements vscode.TreeDataProvider<RunnerRow>, vscode.Disposable {
  private readonly changed = new vscode.EventEmitter<RunnerRow | undefined>();
  readonly onDidChangeTreeData = this.changed.event;

  private treeView?: vscode.TreeView<RunnerRow>;
  private socket?: EventSocket;
  private focusSubscription?: vscode.Disposable;
  private statusEvent?: Event;
  private statusBody?: RunnerStatusBody | null;
  private adminState: AdminState = 'unknown';
  private deadLetters: DeadLetterItem[] = [];
  private lastRunsJson?: string;
  private refreshTimer?: NodeJS.Timeout;
  private healthTimer?: NodeJS.Timeout;
  private intervalMs = ESCUREL_RUNNER_STATUS_INTERVAL_MS;
  private readonly disposables: vscode.Disposable[] = [];
  private isDisposed = false;
  private isFetching = false;
  private pendingRefetch = false;
  /** Retires a fetch that was started for a gateway or tenant the user has since left. */
  private readonly loads = latest();
  private cachedRows: RunnerRow[] = [];

  constructor(private readonly services: Services) {
    // Listen to admin status changes to re-render (e.g. Quotas row visibility)
    this.disposables.push(
      this.services.admin.onDidChange(async () => {
        this.adminState = await this.services.admin.get().catch(() => 'unknown' as const);
        this.rebuildRows();
        this.changed.fire(undefined);
      }),
    );

    // Rebuild socket when auth/connection state changes
    this.disposables.push(
      this.services.onDidChange(() => {
        // A different gateway or tenant: nothing cached belongs to it. Left in place, the previous
        // tenant's dead letters would show under the new runner whenever its run counts happened
        // to match, because the dead letters are only refetched when the counts change.
        this.statusEvent = undefined;
        this.statusBody = undefined;
        this.deadLetters = [];
        this.lastRunsJson = undefined;
        // A fetch already in flight asked the PREVIOUS gateway; its answer must not land here.
        this.loads.invalidate();
        this.rebuildSocket();
        void this.refresh();
      }),
    );

    this.rebuildSocket();

    // Health is a function of NOW. A runner that dies after one good heartbeat sends nothing more,
    // so without a redraw the row would say `ok` for ever. This only re-derives from what is
    // already held; it fetches nothing.
    this.healthTimer = setInterval(() => {
      if (this.isDisposed || !this.statusEvent) return;
      this.rebuildRows();
      this.changed.fire(undefined);
    }, HEALTH_REDRAW_MS);
  }

  bindView(treeView: vscode.TreeView<RunnerRow>): void {
    this.treeView = treeView;
    this.updateMessage();
  }

  getTreeItem(element: RunnerRow): vscode.TreeItem {
    const collapsible =
      element.collapsibleState === 'expanded'
        ? vscode.TreeItemCollapsibleState.Expanded
        : element.collapsibleState === 'collapsed'
          ? vscode.TreeItemCollapsibleState.Collapsed
          : vscode.TreeItemCollapsibleState.None;

    const item = new vscode.TreeItem(element.label, collapsible);
    item.description = element.description;
    if (element.tooltip) item.tooltip = element.tooltip;

    // Apply specific icons and context values based on row kind
    switch (element.kind) {
      case 'health': {
        const desc = element.description ?? '';
        if (desc.includes('draining')) {
          item.iconPath = new vscode.ThemeIcon('sync~spin', new vscode.ThemeColor('charts.yellow'));
        } else if (desc.includes('stale')) {
          item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('charts.yellow'));
        } else if (desc.includes('ok')) {
          item.iconPath = new vscode.ThemeIcon('pass', new vscode.ThemeColor('charts.green'));
        } else {
          item.iconPath = new vscode.ThemeIcon('circle-outline');
        }
        break;
      }

      case 'runner':
        item.iconPath = new vscode.ThemeIcon('server');
        break;

      case 'runs':
        item.iconPath = new vscode.ThemeIcon('play');
        break;

      case 'throttled':
        item.iconPath = new vscode.ThemeIcon('dashboard', new vscode.ThemeColor('charts.yellow'));
        break;

      case 'paused':
        item.iconPath = new vscode.ThemeIcon('debug-pause', new vscode.ThemeColor('charts.yellow'));
        break;

      case 'pausedTenant':
        item.iconPath = new vscode.ThemeIcon('organization');
        item.contextValue = 'paused';
        break;

      case 'permits':
        item.iconPath = new vscode.ThemeIcon('key');
        break;

      case 'liveRuns':
        item.iconPath = new vscode.ThemeIcon('pulse');
        break;

      case 'liveRun':
        item.iconPath = new vscode.ThemeIcon('play-circle', new vscode.ThemeColor('charts.green'));
        item.contextValue = 'liveRun';
        if (element.runId) {
          item.command = {
            command: 'escurel.openRun',
            title: 'Open Run',
            arguments: [element.runId],
          };
        }
        break;

      case 'deadLetters':
        item.iconPath = new vscode.ThemeIcon('mail');
        break;

      case 'deadLetter':
        item.iconPath = new vscode.ThemeIcon('error', new vscode.ThemeColor('errorForeground'));
        item.contextValue = 'deadLetter';
        if (element.runId) {
          item.command = {
            command: 'escurel.openRun',
            title: 'Open Run',
            arguments: [element.runId],
          };
        }
        break;

      case 'quotas':
        item.iconPath = new vscode.ThemeIcon('pie-chart');
        break;
    }

    if (element.contextValue) {
      item.contextValue = element.contextValue;
    }

    return item;
  }

  async getChildren(element?: RunnerRow): Promise<RunnerRow[]> {
    if (element) {
      return element.children ?? [];
    }

    if (this.cachedRows.length === 0) {
      await this.loadData();
    }
    return this.cachedRows;
  }

  async refresh(): Promise<void> {
    await this.loadData();
    this.changed.fire(undefined);
  }

  private updateMessage(): void {
    if (!this.treeView) return;
    if (!this.statusBody) {
      this.treeView.message = 'No runner has reported yet.';
    } else {
      this.treeView.message = undefined;
    }
  }

  private rebuildRows(): void {
    const admin = this.adminState;

    // Quotas: as decided by owner, response shape for admin_quota numbers is not recorded
    // in test fixtures (returns "no quota manager wired on this server" or "admin_required").
    // We never invent numbers; omitting Quotas row when shape cannot be verified.
    const quotas: Record<string, unknown> | undefined = undefined;

    this.cachedRows = buildRunnerRows(this.statusEvent ?? this.statusBody, this.deadLetters, {
      admin,
      quotas,
      intervalMs: this.intervalMs,
    });
  }

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

      // Update admin state
      try {
        this.adminState = await this.services.admin.get();
      } catch (err) {
        log().debug(`runner view: error fetching admin state: ${describeError(err)}`);
      }
      if (!this.loads.isCurrent(mine)) return;

      // 1. Fetch newest runner status event
      let statusEvent: Event | undefined;
      try {
        // Several rows, not one: the gaps between heartbeats show the runner's own interval.
        const page = await client.listEvents({
          label_skill: 'escurel:runner-status',
          newest_first: true,
          include_system: true,
          limit: 8,
        });
        statusEvent = page.events?.[0];
        this.intervalMs = estimateHeartbeatIntervalMs(page.events ?? []);
      } catch (err) {
        log().debug(`runner view: error fetching runner status: ${describeError(err)}`);
      }
      if (!this.loads.isCurrent(mine)) return;

      this.statusEvent = statusEvent;
      this.statusBody = parseRunnerStatusBody(statusEvent);
      this.updateMessage();

      // 2. Check if runs counts changed, and refetch dead letters if needed
      const currentRunsJson = JSON.stringify(this.statusBody?.runs ?? {});
      const runsChanged = this.lastRunsJson !== currentRunsJson;
      if (runsChanged || this.deadLetters.length === 0) {
        this.lastRunsJson = currentRunsJson;
        try {
          const deadLetters = await this.fetchDeadLetters(client);
          if (!this.loads.isCurrent(mine)) return;
          this.deadLetters = deadLetters;
        } catch (err) {
          log().debug(`runner view: error fetching dead letters: ${describeError(err)}`);
        }
      }

      // 3. Rebuild view rows
      this.rebuildRows();
    } finally {
      this.isFetching = false;
      if (this.pendingRefetch) {
        this.pendingRefetch = false;
        await this.loadData();
      }
    }
  }

  /**
   * Fetches the newest dead-lettered runs from escurel:run events.
   * Keeps run-finished rows whose status is dead_letter.
   * Scans up to 300 rows or until 20 dead letters are found (as per owner decision).
   */
  private async fetchDeadLetters(
    client: EscurelClient,
    targetCount = 20,
    maxRows = 300,
  ): Promise<DeadLetterItem[]> {
    const collected: Event[] = [];
    let cursor: string | undefined = undefined;
    let inspected = 0;

    while (inspected < maxRows) {
      const pageSize = Math.min(50, maxRows - inspected);
      const page: EventsPage = await client.listEvents({
        label_skill: 'escurel:run',
        newest_first: true,
        include_system: true,
        limit: pageSize,
        cursor,
      });

      if (!page.events || page.events.length === 0) break;
      collected.push(...page.events);
      inspected += page.events.length;

      const items = extractDeadLetters(collected, targetCount);
      if (items.length >= targetCount) {
        return items.slice(0, targetCount);
      }

      if (!page.has_more || !page.next_cursor) break;
      cursor = page.next_cursor;
    }

    return extractDeadLetters(collected, targetCount);
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => {
      void this.refresh();
    }, 300);
  }

  private rebuildSocket(): void {
    if (this.isDisposed) return;
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    this.focusSubscription = undefined;

    if (!this.services.gatewayUrl) return;

    // Connect EventSocket with filter { label_skill: 'escurel:runner-status' }.
    // As confirmed by testing ws_event_filters.rs, include_system is not supported on /ws
    // and omitting kind allows all kinds (including system events) to pass through.
    this.socket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      filters: { label_skill: 'escurel:runner-status' },
      onEvent: () => {
        this.scheduleRefresh();
      },
      onConnect: () => {
        void this.refresh();
      },
      onWarning: (kind, message) => {
        if (kind === 'session_cap_reached') {
          log().warn(`runner view: ${message}; falling back to refresh-on-focus`);
          this.socket?.close();
          this.focusSubscription ??= vscode.window.onDidChangeWindowState((s) => {
            if (s.focused) void this.refresh();
          });
          this.disposables.push(this.focusSubscription);
          return;
        }
        log().warn(`runner view: live warning: ${message}`);
        void this.refresh();
      },
      onError: (err) => {
        log().warn(`runner view: live error: ${describeError(err)}`);
      },
    });

    this.socket.connect();
  }

  dispose(): void {
    this.isDisposed = true;
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    if (this.healthTimer) clearInterval(this.healthTimer);
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    for (const d of this.disposables) d.dispose();
    this.disposables.length = 0;
  }
}

/**
 * Registers the Runner tree view and associated commands.
 */
export function registerRunnerView(
  context: vscode.ExtensionContext,
  services: Services,
  viewOrTree?: RunnerTree | vscode.TreeView<RunnerRow>,
): RunnerTree {
  let runnerTree: RunnerTree;
  let runnerView: vscode.TreeView<RunnerRow>;

  if (viewOrTree instanceof RunnerTree) {
    runnerTree = viewOrTree;
    runnerView = vscode.window.createTreeView('escurel.runner', {
      treeDataProvider: runnerTree,
      showCollapseAll: false,
    });
    runnerTree.bindView(runnerView);
    context.subscriptions.push(runnerView);
  } else if (viewOrTree) {
    runnerView = viewOrTree;
    runnerTree = new RunnerTree(services);
    runnerTree.bindView(runnerView);
    context.subscriptions.push(runnerTree);
  } else {
    runnerTree = new RunnerTree(services);
    runnerView = vscode.window.createTreeView('escurel.runner', {
      treeDataProvider: runnerTree,
      showCollapseAll: false,
    });
    runnerTree.bindView(runnerView);
    context.subscriptions.push(runnerView, runnerTree);
  }

  context.subscriptions.push();

  return runnerTree;
}
