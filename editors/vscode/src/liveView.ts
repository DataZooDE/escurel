import * as vscode from 'vscode';
import type { Event } from './client';
import type { EventFilters } from './client/ws';
import { EventSocket } from './client/ws';
import { describeError } from './errors';
import { log } from './log';
import type { Services } from './services';

/**
 * One view's own live subscription (SPEC §1: "each view owns its own WS with its
 * own `event_subscribe` filter and resumes with `since_event_id`").
 *
 * Separate from the queue coordinator in `live.ts` because the two resume
 * DIFFERENTLY, and the difference is not a detail. A lineage-scoped subscription
 * — one filtering on `root_event_id` or `run_id`, and only those two — resumes by
 * LOG POSITION and replays events of any status, which is what a thread needs:
 * run events carry deterministic ids (`run:X:finished` sorts below
 * `run:X:started`), so resuming by id comparison would skip a run's terminal for
 * ever. Every other subscription resumes inbox-only, which is why the queues
 * refetch on connect instead.
 *
 * An unknown `since_event_id` replays the WHOLE lineage. That is fine and it is
 * the reason this class never dedupes for itself: `EventSocket` already drops a
 * repeated `event_id`.
 */
export class LiveViewSocket implements vscode.Disposable {
  private socket?: EventSocket;
  private focusSubscription?: vscode.Disposable;
  private readonly disposables: vscode.Disposable[] = [];
  private disposed = false;
  private lastEventId?: string;

  /**
   * @param filters Lineage-scoped: `{ root_event_id }` for a thread,
   *   `{ run_id }` for run detail. Anything else resumes inbox-only, so this
   *   class refuses it rather than degrade silently.
   * @param onEvent An event this view should fold in.
   * @param onResync Refetch from scratch: the stream skipped events, or a
   *   reconnect may have missed some.
   */
  constructor(
    private readonly services: Services,
    private readonly filters: { root_event_id: string } | { run_id: string },
    private readonly onEvent: (event: Event) => void,
    private readonly onResync: () => void,
  ) {
    if (!('root_event_id' in filters) && !('run_id' in filters)) {
      throw new Error('LiveViewSocket: filters must name root_event_id or run_id');
    }
    this.disposables.push(this.services.onDidChange(() => this.rebuild()));
    this.rebuild();
  }

  get state(): string {
    return this.socket?.state ?? 'none';
  }

  dispose(): void {
    this.disposed = true;
    this.socket?.close();
    this.socket = undefined;
    this.focusSubscription?.dispose();
    for (const d of this.disposables) d.dispose();
    this.disposables.length = 0;
  }

  private rebuild(): void {
    if (this.disposed) return;
    this.socket?.close();
    this.focusSubscription?.dispose();
    this.focusSubscription = undefined;

    this.socket = new EventSocket({
      gatewayUrl: this.services.gatewayUrl,
      tokens: this.services.auth.refresher,
      filters: this.filters as EventFilters,
      ...(this.lastEventId ? { sinceEventId: this.lastEventId } : {}),
      onEvent: (event) => {
        this.lastEventId = event.event_id;
        this.onEvent(event);
      },
      onConnect: () => this.onResync(),
      onWarning: (kind, message) => {
        if (kind === 'session_cap_reached') {
          // The tenant is at its concurrent-session cap and this view is the one
          // that lost. Not the user's problem to solve, so it is logged and the
          // view falls back to refreshing when the window regains focus.
          log().warn(`live view: ${message}; falling back to refresh-on-focus`);
          this.socket?.close();
          this.focusSubscription ??= vscode.window.onDidChangeWindowState((s) => {
            if (s.focused) this.onResync();
          });
          this.disposables.push(this.focusSubscription);
          return;
        }
        // Lagged: the stream skipped events, so what is on screen is stale in a
        // way no single event can repair.
        log().warn(`live view: ${message}`);
        this.onResync();
      },
      onError: (err) => log().warn(`live view: ${describeError(err)}`),
    });
    this.socket.connect();
  }
}
