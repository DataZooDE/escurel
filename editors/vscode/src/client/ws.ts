import WebSocket from 'ws';
import type { TokenSource } from '../auth/tokenSource';
import { EscurelError } from './errors';
import type { Event } from './types';

/** `event_subscribe.filters` (crates/escurel-server/src/ws.rs). All strings. */
export interface EventFilters {
  root_event_id?: string;
  run_id?: string;
  label_skill?: string;
  kind?: 'user' | 'system';
  instance_page_id?: string;
}

export type WarningKind = 'lagged' | 'session_cap_reached';

export interface EventSocketOptions {
  gatewayUrl: string;
  tokens: TokenSource;
  filters?: EventFilters;
  /** Resume point for the first subscribe (the last `event_id` the view saw). */
  sinceEventId?: string;
  onEvent: (event: Event, replayed: boolean) => void;
  /** Non-blocking: `lagged` = poll to reconcile; `session_cap_reached` = refresh-on-focus from now on. */
  onWarning: (kind: WarningKind, message: string) => void;
  onError: (error: EscurelError) => void;
  /** Fired on every successful open and subscription frame send. */
  onConnect?: () => void;
  reconnect?: { minMs: number; maxMs: number };
}

export type SocketState = 'idle' | 'connecting' | 'open' | 'closed' | 'capped';

/**
 * One view's live subscription: one socket, one `event_subscribe` (the
 * gateway allows exactly one per socket), resumed with `since_event_id`
 * after a drop and reconnected with the new bearer after a token refresh.
 * Sockets live in the extension host because the bearer rides the HTTP
 * upgrade — a webview's `WebSocket` cannot set that header.
 */
export class EventSocket {
  private ws?: WebSocket;
  private subscriptionId = 0;
  private attempt = 0;
  private timer?: NodeJS.Timeout;
  private readonly seen = new Set<string>();
  private readonly seenOrder: string[] = [];
  private refreshSub?: { dispose(): void };
  private closedByUs = false;
  private _state: SocketState = 'idle';
  private _lastEventId?: string;

  constructor(private readonly opts: EventSocketOptions) {
    this._lastEventId = opts.sinceEventId;
  }

  get state(): SocketState {
    return this._state;
  }

  /** The last event id delivered (or the initial resume point). */
  get lastEventId(): string | undefined {
    return this._lastEventId;
  }

  connect(): void {
    this.closedByUs = false;
    this.refreshSub ??= this.opts.tokens.onDidRefresh?.(() => this.reconnectNow());
    void this.open();
  }

  close(): void {
    this.closedByUs = true;
    this._state = 'closed';
    this.refreshSub?.dispose();
    this.refreshSub = undefined;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    this.ws?.removeAllListeners();
    this.ws?.close();
    this.ws = undefined;
  }

  private reconnectNow(): void {
    if (this.closedByUs) return;
    this.attempt = 0;
    this.ws?.removeAllListeners();
    this.ws?.close();
    this.ws = undefined;
    void this.open();
  }

  private async open(): Promise<void> {
    if (this.closedByUs) return;
    this._state = 'connecting';
    const headers: Record<string, string> = {};
    const token = await this.opts.tokens.get();
    if (token) headers.authorization = `Bearer ${token}`;
    const url = new URL('/ws', this.opts.gatewayUrl.replace(/\/+$/, '') + '/');
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(url, { headers });
    this.ws = ws;

    ws.on('unexpected-response', (_req, res) => {
      let raw = '';
      res.on('data', (c) => (raw += c));
      res.on('end', () => {
        let body: { error?: string; message?: string } | undefined;
        try {
          body = JSON.parse(raw);
        } catch {
          body = undefined;
        }
        const err = EscurelError.fromHttp(res.statusCode ?? 0, body);
        ws.removeAllListeners();
        this.ws = undefined;
        if (err.kind === 'session_cap_reached') {
          this._state = 'capped';
          this.opts.onWarning('session_cap_reached', err.message);
          return;
        }
        if (
          err.kind === 'unauthorized' ||
          err.kind === 'forbidden' ||
          err.kind === 'tenant_suspended'
        ) {
          this._state = 'closed';
          this.opts.onError(err);
          return;
        }
        this.scheduleReconnect();
      });
    });

    ws.on('open', () => {
      this._state = 'open';
      this.attempt = 0;
      ws.send(JSON.stringify({ type: 'hello', presence_only: true }));
      const frame: Record<string, unknown> = {
        type: 'event_subscribe',
        subscription_id: ++this.subscriptionId,
      };
      if (this._lastEventId) frame.since_event_id = this._lastEventId;
      if (this.opts.filters && Object.keys(this.opts.filters).length)
        frame.filters = this.opts.filters;
      ws.send(JSON.stringify(frame));
      this.opts.onConnect?.();
    });

    ws.on('message', (data) => this.onFrame(JSON.parse(String(data))));

    ws.on('error', (e) => {
      // A refused upgrade also emits `error`; `unexpected-response` handles
      // those, and the close that follows drives the reconnect.
      if (this._state === 'open') this.opts.onError(new EscurelError('transport', e.message));
    });

    ws.on('close', () => {
      if (this.ws !== ws) return;
      this.ws = undefined;
      if (this.closedByUs || this._state === 'capped' || this._state === 'closed') return;
      this.scheduleReconnect();
    });
  }

  private onFrame(frame: Record<string, unknown>): void {
    switch (frame.type) {
      case 'event': {
        const event = frame.event as Event;
        // Subscribe happens before the replay query on the gateway, so an
        // event can arrive twice: dedupe by id (bounded).
        if (this.seen.has(event.event_id)) return;
        this.seen.add(event.event_id);
        this.seenOrder.push(event.event_id);
        if (this.seenOrder.length > 4096) this.seen.delete(this.seenOrder.shift()!);
        this._lastEventId = event.event_id;
        this.opts.onEvent(event, frame.replayed === true);
        return;
      }
      case 'event_lagged':
        this.opts.onWarning('lagged', String(frame.message ?? 'fell behind the event stream'));
        return;
      case 'error':
        this.opts.onError(
          new EscurelError('rpc', String(frame.message ?? frame.code), { data: frame }),
        );
        return;
      default:
        return; // acks, presence echoes
    }
  }

  private scheduleReconnect(): void {
    if (this.closedByUs) return;
    const { minMs, maxMs } = this.opts.reconnect ?? { minMs: 1000, maxMs: 30_000 };
    const delay = Math.min(maxMs, minMs * 2 ** Math.min(this.attempt++, 10));
    this._state = 'connecting';
    this.timer = setTimeout(() => void this.open(), delay);
  }
}
