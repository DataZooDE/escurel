import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { McpError } from '@modelcontextprotocol/sdk/types.js';
import type { TokenSource } from '../auth/tokenSource';
import { EscurelError } from './errors';
import { createTransport } from './transport';
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
    const payload = (result.structuredContent ?? {}) as Record<string, unknown>;
    if (tool !== 'validate' && (result.isError || payload.ok === false))
      throw EscurelError.fromPayload(tool, payload);
    return payload as T;
  }

  // ── catalogue + pages ────────────────────────────────────────────

  async listSkills(): Promise<Skill[]> {
    return (await this.call<{ skills: Skill[] }>('list_skills', {})).skills;
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
