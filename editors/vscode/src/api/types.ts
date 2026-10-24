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

export interface HelperCredentialRequest {
  /** The caller's own extension id (`publisher.name`). Checked against the allowlist or the user's consent. */
  extensionId: string;
  /** What the helper is, for the run record: 1 to 40 letters, digits, `_`, `-`; the helper acts as `agent:helper-<audience>`. */
  audience: string;
  /** Reserved. The gateway cannot narrow a credential by scope yet, so only an empty list is accepted. */
  scopes?: readonly string[];
  /** Lifetime in seconds: whole number, 1 to 3600, default 300. */
  ttlSeconds?: number;
}

/**
 * A short-lived bearer minted for a helper process. Pass `token` to the helper by environment or stdin,
 * never on a command line, and do not store it. `token` is not enumerable: it does not appear in JSON or
 * a log of this object. `undefined` on a gateway without a verifier, where the helper needs none.
 */
export interface HelperCredential {
  readonly token: string | undefined;
  /** RFC 3339 expiry of the token. */
  readonly expiresAt: string | undefined;
  /** The gateway the helper talks to (the one this window is connected to). */
  readonly gatewayUrl: string;
}

/** A font file the companion ships. It is served from the webview's own origin only. */
export interface WebviewFont {
  /** CSS family name: 1 to 60 letters, digits, spaces, `_`, `-`. */
  family: string;
  /** A `.woff2`, `.woff`, `.ttf` or `.otf` under the companion's `extensionUri`. The companion's webview `localResourceRoots` must include its folder. */
  file: vscode.Uri;
  /** 100 to 900 in steps of 100. Default 400. */
  weight?: number;
  /** Default `normal`. */
  style?: 'normal' | 'italic';
}

/** Optional parts of the shell. All validated; none loosens the CSP beyond `font-src` for the webview origin. */
export interface WebviewShellOptions {
  /** Adds the `@font-face` rules and `font-src <webview origin>`. At most 8. */
  fonts?: readonly WebviewFont[];
  /** `<html lang>`: a language tag such as `de` or `pt-BR` (for example `vscode.env.language`). */
  lang?: string;
  /** `<title>`: at most 200 characters, escaped. */
  title?: string;
}

/**
 * The calm-window settings a companion wants when the user turns focus mode on. Data only. Appearance
 * settings only: `window.*`, `workbench.*`, `breadcrumbs.*`, `editor.minimap.*` and a few editor looks.
 */
export interface FocusProfile {
  /** Unique: 1 to 64 letters, digits, `.`, `_`, `-`; `escurel` is reserved. The user selects it by this id. */
  id: string;
  /** Setting id to value, at most 40, plain JSON values. The profile replaces Escurel's own list while active. */
  settings: Readonly<Record<string, unknown>>;
}

export interface WebviewHtmlOptions extends WebviewShellOptions {
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
  /**
   * The signed-in subject (the `sub` claim) as a string, never the token. `undefined` on a gateway
   * without a verifier or with no session. A change of identity fires `onDidChangeTenant`: ask again.
   */
  getSubject(): Promise<string | undefined>;
  /**
   * A short-lived credential for a helper process the companion starts. The user's own token stays in the
   * host: the gateway mints a new bearer (same tenant, same person, never admin, no refresh). Refused unless
   * the extension is in `escurel.api.helperCredentialExtensions` or the user accepted a one-time question.
   */
  getHelperCredential(request: HelperCredentialRequest): Promise<HelperCredential>;
  /**
   * Offers a focus-mode profile. Escurel's single focus toggle applies the ACTIVE profile and keeps the
   * person's own values: the user's setting `escurel.focus.profile` picks it; unset, the one registered
   * profile is used, and with several the built-in one stays until the user chooses. Duplicate id throws.
   */
  registerFocusProfile(profile: FocusProfile): vscode.Disposable;
  /** The shell of a companion webview: CSP, nonce, theme tokens. */
  webviewHtml(options: WebviewHtmlOptions): string;
  readonly themeTokens: ThemeTokens;
  readonly themeCss: string;
  registerOverviewTile(tile: OverviewTileContribution): vscode.Disposable;
  registerPageRenderer(skill: string, renderer: PageRenderer): vscode.Disposable;
  /** Adds read-only sections, buttons and `create_draft` forms to the generic page view of `skill`. */
  registerPageAugment(skill: string, augment: PageAugment): vscode.Disposable;
  /** Hooks around the review decision (accept, reject, accept with edits) and a preview beside the diff. */
  registerReviewHook(hook: ReviewHook): vscode.Disposable;
  /** Decorates the Inbox rows of `skill` (badge, secondary text, status) and contributes an empty-state message. */
  registerInboxDecorator(skill: string, decorator: InboxDecorator): vscode.Disposable;
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

// ── review decision hooks ────────────────────────────────────────────

export type ReviewDecision = 'accepted' | 'rejected' | 'accepted_with_edits';

/** A proposal under decision, by ids only. */
export interface ReviewDraftRef {
  draftId: string;
  targetPageId: string;
}

export interface ReviewBeforeContext {
  decision: ReviewDecision;
  /** One draft, or every open draft of a changeset. */
  drafts: ReviewDraftRef[];
  client: GatewayClient;
  /** Aborted when the review stops waiting (about 5 s). */
  signal?: AbortSignal;
}

export interface ReviewBeforeResult {
  /** Ask the reviewer for a note: `required` refuses an empty one. Default: no question. */
  note?: 'required' | 'optional';
  /** The question, one line. */
  notePrompt?: string;
  /** A non-empty message stops the decision and is shown to the reviewer. Only this blocks. */
  veto?: string;
}

export interface ReviewAfterContext {
  draftId: string;
  targetPageId: string;
  decision: ReviewDecision;
  /** What the reviewer wrote; empty when no note was asked or given. */
  note: string;
  /** The proposal's own provenance (author, run, thread, changeset, creation time), string values only. */
  provenance: Record<string, string>;
  client: GatewayClient;
}

export interface ReviewPreviewContext {
  draftId: string;
  targetPageId: string;
  client: GatewayClient;
  signal?: AbortSignal;
}

export interface ReviewHook {
  /**
   * Before accept, reject or accept-with-edits: may ask for a decision note and may veto. An error or a
   * timeout is ignored (logged); only an explicit `veto` blocks.
   */
  before?(ctx: ReviewBeforeContext): ReviewBeforeResult | void | Promise<ReviewBeforeResult | void>;
  /** After the decision was recorded, once per decided draft: e.g. store an event. Errors are ignored. */
  after?(ctx: ReviewAfterContext): void | Promise<void>;
  /**
   * Sections shown beside the diff of a draft (same declarative schema as the page augment, read-only;
   * list lines may open a host command). Return `[]` for a draft that is not yours.
   */
  preview?(ctx: ReviewPreviewContext): AugmentSectionData[] | Promise<AugmentSectionData[]>;
}

// ── inbox row decoration ─────────────────────────────────────────────

export type InboxRowStatus = 'open' | 'answered' | 'failed' | 'done';

/** An inbox row as a decorator sees it: strings only (`''` when the event does not have the value). */
export interface InboxRowData {
  eventId: string;
  labelSkill: string;
  title: string;
  body: string;
  pageId: string;
  /** RFC 3339 instant of the capture. */
  at: string;
  rootEventId: string;
  runId: string;
  /** The event's own status, `inbox` or `processed`. */
  eventStatus: string;
  /** The event's provenance, string values only. */
  provenance: Record<string, string>;
}

/** Data only: nothing here runs in a webview or names a command. */
export interface InboxDecoration {
  /** A short word before the secondary text (at most 24 characters), e.g. `Answered`. */
  badge?: string;
  /** Replaces the secondary text of the row (the skill and the age). */
  description?: string;
  /** Picks the icon; a row that is not `open` sorts after the open ones. `open` rows count in the view badge. */
  status?: InboxRowStatus;
  /** Leaves the row out (a technical receipt that is not work for a person). */
  hidden?: boolean;
}

export interface InboxDecorator {
  /**
   * Called once per row of the skill with every visible row for correlation (a reply row can mark its
   * request answered). An error or a timeout leaves the row as it was.
   */
  decorate?(
    row: InboxRowData,
    rows: readonly InboxRowData[],
  ): InboxDecoration | undefined | Promise<InboxDecoration | undefined>;
  /**
   * Said in the Inbox view when it has no row, so a requester sees where a request goes. A plain string,
   * or one text per language (`en`, `de`, ...); the first decorator that contributes one is used.
   */
  emptyMessage?: string | Record<string, string>;
}
