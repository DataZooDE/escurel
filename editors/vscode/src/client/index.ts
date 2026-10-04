import { checkSkillsCompatible } from './compat';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { McpError } from '@modelcontextprotocol/sdk/types.js';
import type { ToolInfo } from '../auth/adminState';
import type { TokenSource } from '../auth/tokenSource';
import { EscurelError } from './errors';
import { authedFetch, createTransport, mcpUrl } from './transport';
import type {
  ApplyOpRequest,
  ApplyOpResponse,
  CaptureEventRequest,
  Changeset,
  CloseSessionRequest,
  CloseSessionResponse,
  CreateDraftRequest,
  CreateDraftResponse,
  DiffDraftResponse,
  Draft,
  Event,
  EventsPage,
  ExpandRequest,
  ExpandResponse,
  FetchBlobResponse,
  GetRunToolCallsRequest,
  GetRunToolCallsResponse,
  ListEventsRequest,
  ListInboxRequest,
  ListInstancesRequest,
  ListInstancesResponse,
  ListLineageRequest,
  ListLineageResponse,
  MintAgentTokenRequest,
  MintAgentTokenResponse,
  OpenSessionRequest,
  OpenSessionResponse,
  PromoteChangesetResponse,
  PromoteDraftResponse,
  ReportProgressRequest,
  ResolveResponse,
  SearchRequest,
  SearchResponse,
  Skill,
  UpdatePageRequest,
  UpdatePageResponse,
  ValidateRequest,
  ValidateResponse,
} from './types';

export * from './types';
export { EscurelError } from './errors';
export type { ErrorKind } from './errors';

export interface EscurelClientOptions {
  gatewayUrl: string;
  tokens: TokenSource;
}

/**
 * The typed wrapper: one function per tool, and the ONLY place in the
 * extension that knows a tool's name or argument shape (SPEC §1). Connects
 * lazily; a 401 drops the connection so the next call reconnects with the
 * refreshed bearer.
 */
export class EscurelClient {
  private client?: Client;
  private connecting?: Promise<Client>;

  constructor(private readonly opts: EscurelClientOptions) {}

  private async connected(): Promise<Client> {
    if (this.client) return this.client;
    this.connecting ??= (async () => {
      const client = new Client({ name: 'escurel-vscode', version: '0.1.0' });
      try {
        await client.connect(createTransport(this.opts.gatewayUrl, this.opts.tokens));
      } catch (e) {
        throw this.mapError('initialize', e);
      }
      this.client = client;
      return client;
    })();
    try {
      return await this.connecting;
    } finally {
      this.connecting = undefined;
    }
  }

  async close(): Promise<void> {
    const c = this.client;
    this.client = undefined;
    await c?.close().catch(() => undefined);
  }

  private mapError(tool: string, e: unknown): EscurelError {
    if (e instanceof EscurelError) {
      if (e.kind === 'unauthorized') void this.close();
      return e;
    }
    if (e instanceof McpError) return EscurelError.fromRpc(tool, e.code, e.message, e.data);
    return new EscurelError('transport', e instanceof Error ? e.message : String(e), { tool });
  }

  /** `tools/call`; a `{ok: false}` payload (except from `validate`) is thrown as a typed refusal. */
  private async call<T>(tool: string, args: Record<string, unknown>): Promise<T> {
    const client = await this.connected();
    let result;
    try {
      result = await client.callTool({ name: tool, arguments: args });
    } catch (e) {
      throw this.mapError(tool, e);
    }
    const raw = result as { structuredContent?: unknown; content?: unknown; isError?: unknown };
    const refused = refusalFor(tool, raw);
    if (refused) throw EscurelError.fromPayload(tool, refused);
    return payloadOf(raw) as T;
  }

  /**
   * `tools/list`: what this token may call, each tool tagged with its `scope`. The gateway
   * filters it by role, which is the only thing the extension reads to tell an admin (see
   * `detectAdminState`).
   *
   * A raw request, not the SDK's `listTools()`: the SDK validates each tool against its own
   * schema and drops the fields it does not know, and `scope` is exactly the one needed.
   */
  async listTools(): Promise<ToolInfo[]> {
    const send = authedFetch(this.opts.tokens);
    const tools: ToolInfo[] = [];
    let cursor: string | undefined;
    try {
      do {
        const res = await send(mcpUrl(this.opts.gatewayUrl), {
          method: 'POST',
          headers: { 'content-type': 'application/json', accept: 'application/json' },
          body: JSON.stringify({
            jsonrpc: '2.0',
            id: 1,
            method: 'tools/list',
            ...(cursor ? { params: { cursor } } : {}),
          }),
        });
        const body = (await res.json()) as {
          result?: { tools?: { name: string; scope?: unknown }[]; nextCursor?: string };
          error?: { code: number; message: string; data?: unknown };
        };
        if (body.error)
          throw EscurelError.fromRpc(
            'tools/list',
            body.error.code,
            body.error.message,
            body.error.data,
          );
        for (const t of body.result?.tools ?? []) {
          tools.push(
            typeof t.scope === 'string' ? { name: t.name, scope: t.scope } : { name: t.name },
          );
        }
        cursor = body.result?.nextCursor;
      } while (cursor);
    } catch (e) {
      throw this.mapError('tools/list', e);
    }
    return tools;
  }

  // ── catalogue + pages ────────────────────────────────────────────

  async listSkills(): Promise<Skill[]> {
    return checkSkillsCompatible((await this.call<{ skills: Skill[] }>('list_skills', {})).skills);
  }

  listInstancesPage(req: ListInstancesRequest): Promise<ListInstancesResponse> {
    return this.call('list_instances', { ...req });
  }

  /** Walks the cursor until `next_cursor` is `null`. */
  async *listInstances(req: ListInstancesRequest): AsyncGenerator<ListInstancesResponse> {
    let cursor = req.cursor;
    for (;;) {
      const page = await this.listInstancesPage({ ...req, cursor });
      yield page;
      if (page.next_cursor === null || page.next_cursor === undefined) return;
      cursor = page.next_cursor;
    }
  }

  expand(req: ExpandRequest): Promise<ExpandResponse> {
    return this.call('expand', { ...req });
  }

  /** The ORIGINAL file behind a `document` page (base64), or `blob: null` when absent or hidden. */
  fetchBlob(pageId: string): Promise<FetchBlobResponse> {
    return this.call('fetch_blob', { page_id: pageId });
  }

  updatePage(req: UpdatePageRequest): Promise<UpdatePageResponse> {
    return this.call('update_page', { ...req });
  }

  validate(req: ValidateRequest): Promise<ValidateResponse> {
    return this.call('validate', { ...req });
  }

  search(req: SearchRequest): Promise<SearchResponse> {
    return this.call('search', { ...req });
  }

  resolve(req: { wikilink: string }): Promise<ResolveResponse> {
    return this.call('resolve', { ...req });
  }

  // ── events ───────────────────────────────────────────────────────

  listEvents(req: ListEventsRequest): Promise<EventsPage> {
    return this.call('list_events', { ...req });
  }

  listInbox(req: ListInboxRequest = {}): Promise<EventsPage> {
    return this.call('list_inbox', { ...req });
  }

  captureEvent(req: CaptureEventRequest): Promise<Event> {
    return this.call('capture_event', { ...req });
  }

  listLineage(req: ListLineageRequest): Promise<ListLineageResponse> {
    return this.call('list_lineage', { ...req });
  }

  getRunToolCalls(req: GetRunToolCallsRequest): Promise<GetRunToolCallsResponse> {
    return this.call('get_run_tool_calls', { ...req });
  }

  mintAgentToken(req: MintAgentTokenRequest): Promise<MintAgentTokenResponse> {
    return this.call('mint_agent_token', { ...req });
  }

  reportProgress(
    req: ReportProgressRequest,
  ): Promise<{ ok: boolean; event_id: string; run_id: string; steps: number }> {
    return this.call('report_progress', { ...req });
  }

  // ── review ───────────────────────────────────────────────────────

  createDraft(req: CreateDraftRequest): Promise<CreateDraftResponse> {
    return this.call('create_draft', { ...req });
  }

  async listChangesets(limit?: number): Promise<Changeset[]> {
    return (await this.call<{ changesets: Changeset[] }>('list_changesets', limit ? { limit } : {}))
      .changesets;
  }

  async listDrafts(limit?: number): Promise<Draft[]> {
    return (await this.call<{ drafts: Draft[] }>('list_drafts', limit ? { limit } : {})).drafts;
  }

  diffDraft(req: { draft_id: string }): Promise<DiffDraftResponse> {
    return this.call('diff_draft', { ...req });
  }

  promoteDraft(req: { draft_id: string }): Promise<PromoteDraftResponse> {
    return this.call('promote_draft', { ...req });
  }

  discardDraft(req: {
    draft_id: string;
    reason?: string;
  }): Promise<{ ok: true; draft_id: string }> {
    return this.call('discard_draft', { ...req });
  }

  promoteChangeset(req: { changeset_id: string }): Promise<PromoteChangesetResponse> {
    return this.call('promote_changeset', { ...req });
  }

  discardChangeset(req: {
    changeset_id: string;
    reason?: string;
  }): Promise<{ ok: true; changeset_id: string; discarded: number }> {
    return this.call('discard_changeset', { ...req });
  }

  // ── live sessions ──────────────────────────────────────────────────

  openSession(req: OpenSessionRequest): Promise<OpenSessionResponse> {
    return this.call('open_session', { ...req });
  }

  applyOp(req: ApplyOpRequest): Promise<ApplyOpResponse> {
    return this.call('apply_op', { ...req });
  }

  closeSession(req: CloseSessionRequest): Promise<CloseSessionResponse> {
    return this.call('close_session', { ...req });
  }
}

/**
 * The payload of a tool result: `structuredContent` (the full result; current gateways put a short
 * summary in the text block), else, for a LEGACY gateway that sent the payload only as JSON text,
 * that text parsed.
 */
export function payloadOf(result: {
  structuredContent?: unknown;
  content?: unknown;
}): Record<string, unknown> {
  if (result.structuredContent && typeof result.structuredContent === 'object')
    return result.structuredContent as Record<string, unknown>;
  const first = Array.isArray(result.content)
    ? (result.content[0] as { text?: unknown })
    : undefined;
  if (typeof first?.text === 'string') {
    try {
      const parsed: unknown = JSON.parse(first.text);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed))
        return parsed as Record<string, unknown>;
    } catch {
      /* a summary, not JSON */
    }
  }
  return {};
}

/**
 * The payload of a REFUSED tool result (`isError`, or `ok: false`), else `undefined`. A refusal is
 * always an error to the caller, never data: the payload is guaranteed to carry at least one issue,
 * built from the result's own text when the tool named none, so the person is told why.
 */
export function refusalFor(
  tool: string,
  result: { structuredContent?: unknown; content?: unknown; isError?: unknown },
): Record<string, unknown> | undefined {
  // `validate` reports problems with `ok: false` and is not an error: its caller reads the issues. But
  // a validate that FAILED (isError, and no issues to read) is an error: returning its empty payload
  // would read as "no issues" and show a skill clean that was never checked.
  if (tool === 'validate') {
    if (result.isError !== true) return undefined;
    const issues = payloadOf(result).issues;
    return Array.isArray(issues) && issues.length > 0 ? undefined : refusalOf(result);
  }
  return refusalOf(result);
}

export function refusalOf(result: {
  structuredContent?: unknown;
  content?: unknown;
  isError?: unknown;
}): Record<string, unknown> | undefined {
  const payload = payloadOf(result);
  if (result.isError !== true && payload.ok !== false) return undefined;
  const issues = Array.isArray(payload.issues) ? payload.issues : [];
  if (issues.length > 0) return payload;
  const first = Array.isArray(result.content)
    ? (result.content[0] as { text?: unknown })
    : undefined;
  const message =
    typeof first?.text === 'string' && first.text ? first.text : 'the tool refused the call';
  return {
    ...payload,
    ok: false,
    issues: [{ severity: 'error', code: 'tool_error', location: '', message }],
  };
}
