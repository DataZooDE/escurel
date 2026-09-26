import * as vscode from 'vscode';
import type { Event } from './client/types';
import { EventSocket, type SocketState } from './client/ws';
import { describeError } from './errors';
import { log } from './log';
import type { Services } from './services';

export interface StaleViews {
  inbox: boolean;
  awaiting: boolean;
}

export interface LiveViews {
  inbox: { refresh(): void };
  awaiting: { refresh(): void };
}

/**
 * Route an event from the bus to the views that display it.
 *
 * Recorded against a live gateway, because the two look alike and are not:
 * a review TRANSITION (`escurel:review`, titled `draft-created`, `draft-promoted`,
 * `draft-discarded`, `changeset-promoted`, `changeset-discarded`) arrives
 * `kind: system`, `status: processed` — it never reaches `list_inbox`, and it is the
 * only signal that the Awaiting queue moved. Everything the Inbox shows is
 * `kind: user`, `status: inbox`, a review COMMENT included. A system event that is
 * not a review transition belongs to neither queue, so it costs no round trip.
 */
export function staleViews(event: Event): StaleViews {
  if (event.label_skill === 'escurel:review') {
    return { inbox: false, awaiting: true };
  }
  const isInboxEvent = event.kind !== 'system' && (event.status === 'inbox' || !event.status);
  return {
    inbox: isInboxEvent,
    awaiting: false,
  };
}

/**
 * Coalesces multi-event bursts into a single refresh per stale view.
 * When agent runs finish, several drafts or inbox events land in the same tick;
 * debouncing avoids redundant gateway round trips.
 */
export class LiveRefresher implements vscode.Disposable {
  private timer?: NodeJS.Timeout;
  private pendingInbox = false;
  private pendingAwaiting = false;

  constructor(
    private readonly views: LiveViews,
    private readonly debounceMs = 300,
  ) {}

  handleEvent(event: Event): void {
    const stale = staleViews(event);
    if (!stale.inbox && !stale.awaiting) return;
    if (stale.inbox) this.pendingInbox = true;
    if (stale.awaiting) this.pendingAwaiting = true;
    this.schedule();
  }

  /**
   * Refetches both views immediately, clearing any pending debounce so stale
   * timer callbacks do not trigger a duplicate reload.
   */
  refreshBoth(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.pendingInbox = false;
    this.pendingAwaiting = false;
    this.views.inbox.refresh();
    this.views.awaiting.refresh();
  }

  private schedule(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => this.flush(), this.debounceMs);
  }

  flush(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    const doInbox = this.pendingInbox;
    const doAwaiting = this.pendingAwaiting;
    this.pendingInbox = false;
    this.pendingAwaiting = false;

    if (doInbox) this.views.inbox.refresh();
    if (doAwaiting) this.views.awaiting.refresh();
  }

  dispose(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.pendingInbox = false;
    this.pendingAwaiting = false;
  }
}

/**
 * Manages the single unfiltered event socket for live queue updates.
 *
 * The tenant has a concurrent-session cap, so we maintain exactly one socket with
 * no filters and route client-side. When the session cap is hit, we degrade gracefully
 * to refresh-on-focus (SPEC §7) rather than interrupting the user with modals.
 */
export class LiveCoordinator implements vscode.Disposable {
  private socket?: EventSocket;
  private readonly refresher: LiveRefresher;
  private focusSubscription?: vscode.Disposable;
  private readonly disposables: vscode.Disposable[] = [];
  private disposed = false;

  /**
   * The socket's state, so a test can tell a view that refreshed because an
   * event arrived from one that refreshed for some other reason.
   */
  get socketState(): SocketState | 'none' {
    return this.socket?.state ?? 'none';
  }

  constructor(
    private readonly services: Services,
    views: LiveViews,
    opts?: { debounceMs?: number },
  ) {
    this.refresher = new LiveRefresher(views, opts?.debounceMs);
    this.disposables.push(
      this.refresher,
      services.onDidChange(() => this.rebuildSocket()),
    );
    this.rebuildSocket();
  }

  static register(
    context: vscode.ExtensionContext,
    services: Services,
    views: LiveViews,
  ): LiveCoordinator {
    const live = new LiveCoordinator(services, views);
    context.subscriptions.push(live);
    return live;
  }

  get activeSocket(): EventSocket | undefined {
    return this.socket;
  }

  handleEvent(event: Event): void {
    this.refresher.handleEvent(event);
  }

  refreshBoth(): void {
    this.refresher.refreshBoth();
  }

  private rebuildSocket(): void {
    if (this.disposed) return;
    this.socket?.close();
    this.socket = undefined;

    // Reset fallback on session cap so reconfigured settings get a fresh attempt.
    this.focusSubscription?.dispose();
    this.focusSubscription = undefined;

    this.socket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      onEvent: (event) => {
        this.refresher.handleEvent(event);
      },
      onConnect: () => {
        // Replay after a reconnect is inbox-only; review transitions that occurred
        // while disconnected are never replayed. Re-query both views once on connect.
        this.refresher.refreshBoth();
      },
      onWarning: (kind, message) => {
        if (kind === 'lagged') {
          log().warn(`escurel: live event stream lagged (${message}), refreshing queues`);
          this.refresher.refreshBoth();
        } else if (kind === 'session_cap_reached') {
          // Graceful degradation: stop retrying and poll on window focus instead (SPEC §7).
          log().warn(
            `escurel: live subscription reached session cap (${message}); ` +
              'degrading to refresh on window focus',
          );
          this.enableFocusRefresh();
        }
      },
      onError: (err) => {
        // Dead sockets must never disrupt manual refresh or other view interactions.
        log().warn(`escurel: live event socket error: ${describeError(err)}`);
      },
    });
    this.socket.connect();
  }

  private enableFocusRefresh(): void {
    if (this.focusSubscription || this.disposed) return;
    this.focusSubscription = vscode.window.onDidChangeWindowState((state) => {
      if (state.focused) {
        this.refresher.refreshBoth();
      }
    });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    this.focusSubscription = undefined;
    for (const d of this.disposables) d.dispose();
  }
}
