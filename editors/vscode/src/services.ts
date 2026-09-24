import * as vscode from 'vscode';
import { EscurelAuthProvider } from './auth/provider';
import { EscurelClient } from './client';
import { onConfigChange, readConfig } from './config';
import { log } from './log';

/**
 * The per-window wiring: one auth provider, one refresher, one typed client
 * against the configured gateway. Rebuilt when `escurel.*` changes.
 */
export class Services implements vscode.Disposable {
  readonly auth: EscurelAuthProvider;
  private _client: EscurelClient;
  private readonly changed = new vscode.EventEmitter<void>();
  /** Fires when the client was rebuilt (gateway or auth settings changed): views refetch. */
  readonly onDidChange = this.changed.event;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(context: vscode.ExtensionContext) {
    this.auth = new EscurelAuthProvider(context.secrets);
    this._client = this.build();
    this.disposables.push(
      this.auth,
      this.changed,
      onConfigChange(() => {
        void this._client.close();
        this._client = this.build();
        this.changed.fire();
      }),
      vscode.authentication.onDidChangeSessions((e) => {
        if (e.provider.id === 'escurel') {
          void this._client.close();
          this.changed.fire();
        }
      }),
    );
  }

  get client(): EscurelClient {
    return this._client;
  }

  get gatewayUrl(): string {
    return readConfig().gatewayUrl;
  }

  /** `escurel.refresh`: every view refetches. */
  onDidChangeEmit(): void {
    this.changed.fire();
  }

  private build(): EscurelClient {
    const cfg = readConfig();
    log().info(
      `escurel: gateway ${cfg.gatewayUrl}${cfg.auth.issuer ? ` (issuer ${cfg.auth.issuer})` : ' (no issuer configured: no token is sent)'}`,
    );
    return new EscurelClient({ gatewayUrl: cfg.gatewayUrl, tokens: this.auth.refresher });
  }

  dispose(): void {
    void this._client.close();
    for (const d of this.disposables) d.dispose();
  }
}
