// The public contract of the Escurel extension for companion extensions. TYPES ONLY: apart from the
// version constant this file emits nothing, so a companion can `import type` from it without
// bundling any of Escurel. The rules for every member are in docs/design/extension-api.md.
import type * as vscode from 'vscode';
import type { AugmentBlock, AugmentFieldKind, AugmentTone } from '../shared/augment';

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
  /** Adds read-only sections, buttons and `create_draft` forms to the generic page view of `skill`. */
  registerPageAugment(skill: string, augment: PageAugment): vscode.Disposable;
  /** Fires when the gateway or the signed-in identity changed: drop cached data. */
  readonly onDidChangeTenant: vscode.Event<TenantInfo>;
}

// ── page view augment ────────────────────────────────────────────────

export type {
  AugmentTone,
  AugmentBlock,
  AugmentSection,
  AugmentFact,
  AugmentFieldKind,
} from '../shared/augment';

/** A section a companion adds to a page view. List lines may name a host command; the webview never sees it. */
export interface AugmentSectionData {
  /** `[A-Za-z0-9][A-Za-z0-9._-]*`, at most 64 characters, unique within the page. */
  id: string;
  title: string;
  blocks: (
    | Exclude<AugmentBlock, { type: 'list' }>
    | {
        type: 'list';
        items: { label: string; detail?: string; tone?: AugmentTone; command?: string; args?: unknown[] }[];
      }
  )[];
  empty?: string;
}

/** A button that runs a host command. The id is the companion's own; the host hands the webview another. */
export interface AugmentActionData {
  id: string;
  label: string;
  hint?: string;
  command: string;
  args?: unknown[];
}

export interface AugmentFieldData {
  name: string;
  label: string;
  kind: AugmentFieldKind;
  required?: boolean;
  /** Members of an `enum` field. */
  values?: string[];
  /** Characters; default 2000, at most 20000. */
  maxLength?: number;
}

/** The only write a form can make: `create_draft`. The decision stays with a reviewer. */
export interface CreateDraftArgs {
  target_page_id: string;
  content: string;
  base_sha256?: string;
}

export interface AugmentFormData {
  /** Lower case, `[a-z][a-z0-9._-]*`, at most 64 characters. */
  id: string;
  title: string;
  submitLabel?: string;
  fields: AugmentFieldData[];
  /** Host side. Gets the values the host validated; returns the draft to create. */
  toDraft(values: Record<string, unknown>, ctx: PageAugmentContext): CreateDraftArgs | Promise<CreateDraftArgs>;
}

export interface PageAugmentContext {
  pageId: string;
  skill: string;
  frontmatter: Record<string, unknown>;
  client: GatewayClient;
  /** Aborted when the page stops waiting (about 5 s). */
  signal?: AbortSignal;
}

export interface PageAugment {
  /** Read-only sections, drawn by the core webview from the declarative schema. */
  sections?(ctx: PageAugmentContext): AugmentSectionData[] | Promise<AugmentSectionData[]>;
  /** Extra buttons: a host command id and arguments, checked by the host. */
  actions?(ctx: PageAugmentContext): AugmentActionData[] | Promise<AugmentActionData[]>;
  /** Forms whose submit can only call `create_draft`. */
  forms?: AugmentFormData[];
}
