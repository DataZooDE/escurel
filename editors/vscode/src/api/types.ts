// The public contract of the Escurel extension for companion extensions. TYPES ONLY: apart from the
// version constant this file emits nothing, so a companion can `import type` from it without
// bundling any of Escurel. The rules for every member are in docs/design/extension-api.md.
import type * as vscode from 'vscode';

export type {
  PageModel,
  FieldView,
  HostToWebview,
  WebviewToHost,
  OverviewTone,
  OverviewTile,
  OverviewItem,
} from '../shared/protocol';

/** The only version this build speaks. A breaking change bumps it; a companion checks it on activation. */
export const EXTENSION_API_VERSION = 1 as const;

/** The gateway as the signed-in user sees it. The token never leaves the Escurel host. */
export interface GatewayClient {
  /**
   * Calls one MCP tool of the gateway with the user's session. Rejects with the gateway's error; the
   * gateway, not this call, decides what the user may do. `name` is a tool name, `args` a plain object.
   */
  callTool<T = unknown>(name: string, args?: Record<string, unknown>): Promise<T>;
}

/** What identifies the connection a companion's cached data belongs to. */
export interface TenantInfo {
  gatewayUrl: string;
}

export interface WebviewHtmlOptions {
  webview: vscode.Webview;
  /** The companion's own extension URI: its bundle is `dist/webview/<bundle>.js` under it. */
  extensionUri: vscode.Uri;
  /** File stem of the bundle: letters, digits, `-` and `_`. */
  bundle: string;
  /** The custom element the bundle defines: lower case with at least one hyphen. */
  tag: string;
}

export type ThemeTokens = Readonly<Record<string, string>>;

export type OverviewToneName = 'attention' | 'ok' | 'neutral';

export interface OverviewTileLine {
  label: string;
  detail?: string;
  tone?: OverviewToneName;
  /** A command the HOST runs when the line is opened. The webview never sees it. */
  command?: string;
  args?: unknown[];
}

export interface OverviewTileData {
  headline: string;
  tone?: OverviewToneName;
  items?: OverviewTileLine[];
  /** How many more there are than `items` carries. */
  more?: number;
  /** What to say when there are no items. */
  empty?: string;
}

export interface OverviewTileContext {
  client: GatewayClient;
  /** Aborted when the board stops waiting (about 5 s). */
  signal: AbortSignal;
}

export interface OverviewTileContribution {
  /** Unique among contributions: `[a-z0-9][a-z0-9._-]*`, at most 40 characters. */
  id: string;
  title: string;
  /** Opened when the tile's title is clicked (the full view). Optional. */
  openCommand?: string;
  load(ctx: OverviewTileContext): Promise<OverviewTileData>;
}

export interface PageRenderContext {
  pageId: string;
  /** The skill of the page. */
  skill: string;
  frontmatter: Record<string, unknown>;
  /** The editor panel. The renderer sets `webview.options`, `webview.html` (see `webviewHtml`) and the title. */
  panel: vscode.WebviewPanel;
  client: GatewayClient;
  onDidChangeTenant: vscode.Event<TenantInfo>;
}

export interface PageRenderer {
  /** Takes over the panel. If it throws, the generic page view is shown instead. */
  render(ctx: PageRenderContext): vscode.Disposable | void | Promise<vscode.Disposable | void>;
}

export interface EscurelExtensionApi {
  readonly apiVersion: typeof EXTENSION_API_VERSION;
  /** `tenant` is a gateway URL; one window talks to one gateway, so any other value throws. */
  getClient(tenant?: string): GatewayClient;
  /** The shell of a companion webview: CSP, nonce, theme tokens. */
  webviewHtml(options: WebviewHtmlOptions): string;
  readonly themeTokens: ThemeTokens;
  readonly themeCss: string;
  registerOverviewTile(tile: OverviewTileContribution): vscode.Disposable;
  registerPageRenderer(skill: string, renderer: PageRenderer): vscode.Disposable;
  /** Fires when the gateway or the signed-in identity changed: drop cached data. */
  readonly onDidChangeTenant: vscode.Event<TenantInfo>;
}
