// Hand-written from crates/escurel-server/src/mcp/schema.rs (inputs) and the
// handlers' payloads (outputs). Optional wire keys are optional here; a key
// the gateway omits when empty is modelled as optional too.

/** One instance field a skill declares (`list_skills.fields[]`). */
export interface SkillField {
  name: string;
  /** `string | int | float | bool | date | datetime | enum | link` (a string on purpose: forward-compatible). */
  kind: string;
  required: boolean;
  values?: string[];
  target_skill?: string;
  min?: number;
  max?: number;
  label?: string;
  description?: string;
  /** `text | markdown | date | datetime | money | link | badge` — a display hint, passed through verbatim. */
  render?: string;
}

export interface SkillParam {
  name: string;
  kind: string;
  required: boolean;
  label?: string;
  description?: string;
}

export interface SkillBlock {
  anchor: string;
  title?: string;
  kind?: string;
}

export interface Skill {
  id: string;
  description: string;
  required_frontmatter: string[];
  optional_frontmatter: string[];
  is_event_typed: boolean;
  visibility: 'public' | 'owner' | string;
  owner_field: string | null;
  /** Admin callers only; absent for everyone else. */
  acl?: { read?: string[]; create?: string[]; update?: string[]; delete?: string[] };
  backend: { kind: string };
  capabilities: { writable: boolean; granularity: string; search: string; supports_crdt: boolean };
  /** `overlay` (tenant-authored, editable) or `base@<pack>@<version>` (read-only at this node). */
  layer: string;
  shadows?: string;
  /** `auto | review | confirm`; ABSENT when unset or unrecognised — treat absence as "hold for review". */
  autonomy?: string;
  summary?: string;
  harness?: string;
  /** `/`-separated path of slugs placing the skill in a tree (`sales/orders`); absent = top level. */
  folder?: string;
  /** `record | process | report | helper`; typed as a string so a newer server's value still parses. */
  role?: string;
  /** OKF tags. */
  tags?: string[];
  /** OKF display title. */
  title?: string;
  /** OKF link to the external thing this skill describes (a table, an API). */
  resource?: string;
  /** What a follow-up from this skill can be: see `SkillAction`. Absent when none are declared. */
  actions?: SkillAction[];
  cascade?: { target?: string; max_depth?: number };
  params?: SkillParam[];
  fields?: SkillField[];
  blocks?: SkillBlock[];
}

/**
 * One entry of a skill's `actions:` (Peacock's object form). `event` actions name the skill an event
 * is filed under; `prompt` actions are a chat turn in another app. The title/body templates are not
 * on the wire.
 */
export interface SkillAction {
  name: string;
  kind: 'event' | 'prompt';
  label: string;
  event?: string;
  prompt?: string;
}

export interface ListInstancesRequest {
  skill_id: string;
  cursor?: string;
  order_by?: 'at asc' | 'at desc';
  limit?: number;
  frontmatter_key?: string;
  frontmatter_value?: string;
}

export interface Instance {
  page_id: string;
  skill: string;
  frontmatter: Record<string, unknown>;
  at: string | null;
}

export interface ListInstancesResponse {
  instances: Instance[];
  /** Always present; `null` means the last page (a short page is normal — the ACL filter shortens pages). */
  next_cursor: string | null;
}

export interface PageRef {
  page_id: string;
  slug: string | null;
  skill: string;
  page_kind: 'skill' | 'instance' | string;
  last_written_by?: string | null;
}

export interface ExpandRequest {
  page_id: string;
  as_of?: string;
  scenario?: string;
  full?: boolean;
  /** Also return the stored markdown verbatim as `content` (plain reads only). */
  raw?: boolean;
}

export interface WikilinkParsed {
  skill: string | null;
  id: string | null;
  anchor: string | null;
  version: string | null;
  alias: string | null;
}

export interface FetchBlobResponse {
  blob: { content_type: string; size: number; bytes_base64: string } | null;
}

export interface ExpandResponse {
  /** `null` when the page does not exist or the caller may not read it (absence, never a leak). */
  page: PageRef | null;
  frontmatter: Record<string, unknown>;
  body: string;
  blocks: { anchor: string; content: string }[];
  wikilinks_out: WikilinkParsed[];
  /** Hex sha256 of the stored bytes — `update_page.base_sha256`'s guard. Plain reads only. */
  content_sha256?: string;
  /** The stored markdown verbatim; only with `raw: true` on a plain read. */
  content?: string;
  /** Only on a gateway with a live CRDT backend. */
  version?: string;
  shadow?: unknown;
  /** `sql_view`: `{view, rows, source, truncated?, issue?}`; `openapi`/`mcp`: `{source, fields}` or `{issue}`. */
  backend_projection?: unknown;
  /** `document` pages: how many chunks the document has, and whether `blocks` holds only the lead. */
  chunks_total?: number;
  chunks_truncated?: boolean;
}

export interface ValidationIssue {
  severity: 'error' | 'warning' | string;
  code: string;
  location: string;
  message: string;
  suggestion?: string;
}

export interface UpdatePageRequest {
  page_id: string;
  content: string;
  base_sha256?: string;
  base_version?: string;
  require_exact_base?: boolean;
  branch?: string;
  provenance?: Record<string, unknown>;
}

export interface UpdatePageResponse {
  ok: true;
  issues: ValidationIssue[];
  new_version?: string;
  auto_merged?: boolean;
  [key: string]: unknown;
}

export interface ValidateRequest {
  content: string;
  as_page_id?: string;
}

export interface ValidateResponse {
  ok: boolean;
  issues: ValidationIssue[];
}

export interface SearchRequest {
  q: string;
  k?: number;
  granularity?: 'block' | 'page';
  page_kind?: 'skill' | 'instance' | 'any';
  skill?: string;
  filter?: Record<string, unknown>;
}

export interface SearchHit {
  page_id: string;
  slug: string | null;
  skill: string;
  page_kind: string;
  anchor: string | null;
  snippet: string;
  score: number;
  similarity?: number;
  frontmatter_excerpt?: Record<string, unknown>;
}

export interface SearchResponse {
  hits: SearchHit[];
  granularity: string;
}

export interface ResolveResponse {
  parsed: WikilinkParsed;
  page: PageRef | null;
  exists: boolean;
}

export interface Event {
  event_id: string;
  at: string | null;
  source: string | null;
  mime: string | null;
  label_skill: string;
  instance_page_id: string | null;
  status: 'inbox' | 'processed' | string;
  title: string | null;
  body: string | null;
  provenance: Record<string, unknown> | null;
  kind: 'user' | 'system' | string;
  root_event_id: string | null;
  run_id: string | null;
}

export interface ListEventsRequest {
  instance_page_id?: string;
  event_id?: string;
  root_event_id?: string;
  run_id?: string;
  label_skill?: string;
  kind?: 'user' | 'system';
  include_system?: boolean;
  newest_first?: boolean;
  limit?: number;
  cursor?: string;
}

export interface EventsPage {
  events: Event[];
  /**
   * Where THIS page ended; present iff the page is non-empty. Pass it back as `cursor` to continue
   * or to tail. Only its absence means done: a client paging until it is absent makes one extra call
   * that comes back empty. (`resume_cursor`, its old twin, no longer exists.)
   */
  next_cursor?: string;
  /** True iff rows already lie past this page; absent otherwise. */
  has_more?: boolean;
}

export interface ListInboxRequest {
  limit?: number;
  cursor?: string;
  include_system?: boolean;
}

export interface CaptureEventRequest {
  label_skill: string;
  event_id?: string;
  kind?: 'user' | 'system';
  at?: string;
  source?: string;
  mime?: string;
  instance_page_id?: string;
  title?: string;
  body?: string;
  provenance?: Record<string, unknown>;
}

export interface ListLineageRequest {
  root_event_id: string;
  include?: ('events' | 'runs' | 'drafts' | 'tool_calls')[];
  limit?: number;
  cursor?: string;
}

export interface LineageNode {
  id: string;
  type: 'event' | 'run' | 'changeset' | 'draft' | string;
  parent: string | null;
  state: string;
  [attr: string]: unknown;
}

export interface ListLineageResponse {
  root_event_id: string;
  /** Keyed by `id` — merge pages by id, never concatenate. */
  nodes: LineageNode[];
  next_cursor?: string;
}

export interface GetRunToolCallsRequest {
  run_id: string;
  limit?: number;
  /** The `seq` of the last call seen. */
  after?: number;
}

export interface RunToolCall {
  seq: number;
  tool: string;
  status: 'ok' | 'rejected' | 'error' | string;
  error_code: string | null;
  duration_ms: number;
  request_bytes: number;
  response_bytes: number;
  subject: string;
  at: string;
}

export interface GetRunToolCallsResponse {
  run_id: string;
  calls: RunToolCall[];
  next_after: number | null;
}

export interface MintAgentTokenRequest {
  skill: string;
  root_event_id?: string;
  target_page_id?: string;
  ttl_secs?: number;
  trace_id?: string;
}

export interface MintAgentTokenResponse {
  token: string;
  run_id: string;
  root_event_id: string;
  subject: string;
  expires_at: string;
}

export interface ReportProgressRequest {
  plan: { step: string; status: 'pending' | 'in_progress' | 'completed' | 'blocked' }[];
  current?: string;
  note?: string;
}

export interface Draft {
  draft_id: string;
  target_page_id: string;
  content: string;
  content_sha256: string;
  base_sha256: string | null;
  author: string;
  event_id: string | null;
  status: 'open' | 'promoted' | 'discarded' | string;
  reason: string | null;
  decided_by: string | null;
  created_at: string;
  changeset_id: string | null;
  base_version: string | null;
  run_id: string | null;
  root_event_id: string | null;
}

export interface CreateDraftRequest {
  target_page_id: string;
  content: string;
  base_sha256?: string;
  event_id?: string;
  changeset_id?: string;
  new_changeset?: boolean;
}

export interface CreateDraftResponse {
  ok: true;
  draft: Draft;
}

export interface Changeset {
  changeset_id: string;
  drafts: number;
  status: 'open' | 'promoted' | 'discarded' | 'mixed' | string;
  author: string;
  created_at: string;
  run_id: string | null;
  root_event_id: string | null;
  event_ids: string[];
  target_page_ids: string[];
}

export interface DiffDraftResponse {
  ok: true;
  draft_id: string;
  target_page_id: string;
  exists: boolean;
  /** The target moved since drafting: promotion will conflict or need a merge. */
  base_moved: boolean;
  run_id: string | null;
  root_event_id: string | null;
  frontmatter_changes: { key: string; from: unknown; to: unknown }[];
  block_changes: { anchor: string; kind: string; preview: string }[];
}

export interface PromoteDraftResponse {
  ok: true;
  already_applied?: boolean;
  decided_by?: string;
  new_version?: string;
  [key: string]: unknown;
}

export interface PromoteChangesetResponse {
  ok: true;
  changeset_id: string;
  already_decided?: boolean;
  decided_by?: string;
  partial?: boolean;
  results: {
    draft_id: string;
    page_id: string;
    ok?: boolean;
    already_applied?: boolean;
    status?: string;
  }[];
}

export interface OpenSessionRequest {
  page_id?: string;
  draft_id?: string;
}

export interface OpenSessionResponse {
  session: string;
  head_version: string;
  ws_url: string;
  snapshot: string | null;
}

export interface ApplyOpRequest {
  session: string;
  op: string;
}

export interface ApplyOpResponse {
  ok: true;
  merged_version: string;
}

export interface CloseSessionRequest {
  session: string;
  commit?: boolean;
}

export interface CloseSessionResponse {
  ok: true;
  final_version?: string;
  issues?: ValidationIssue[];
  [key: string]: unknown;
}
