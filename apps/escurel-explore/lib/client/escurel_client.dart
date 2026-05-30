/// The contract every escurel backend client implements.
///
/// Mirrors the locked 12-tool agent surface from
/// `docs/contract/agent-interface.md` plus admin MCP tools (gated by
/// the `escurel-admin` role) and the substrate health endpoints.
///
/// Two concrete implementations live alongside this interface:
///
/// - `FixtureEscurelClient` answers from the bundled
///   `examples/crm-demo/` markdown corpus and the read-tool semantics
///   re-implemented in-Dart. Useful for offline demo, CI, and the
///   `--mode=fixture` deployment variant.
/// - `HttpEscurelClient` (PR-6) speaks MCP-over-HTTP at `POST /mcp`
///   on the real escurel-server.
///
/// Both must accept the same DTOs from [`models.dart`] so the
/// editor never branches on transport.
library;

import 'errors.dart';
import 'models.dart';

abstract class EscurelClient {
  // ── read primitives (the 7 tier-1 tools) ────────────────────

  /// Vector + FTS hybrid search; optional `skill=` pushes a
  /// `link_skill` predicate to the index.
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
    String? asOf,
  });

  /// Parse and validate a `[[skill::id]]` reference without
  /// fetching the body. Reports `exists: false` for dangling links
  /// rather than raising. `scenario` resolves against a what-if
  /// overlay (the overlay page wins over its base twin).
  Future<ResolveResult> resolve(String wikilink, {String? scenario});

  /// Fetch the body (and blocks + outgoing wikilinks) for one page.
  /// `asOf` time-travels the read: a page born after the cut resolves
  /// to a not-found result. `scenario` reads against a what-if overlay
  /// (base only when null).
  Future<ExpandResult> expand(String pageId,
      {String? anchor, String? version, String? asOf, String? scenario});

  /// The link-graph primitive: backlinks (`incoming`), forward-links
  /// (`outgoing`), or both. `linkSkill` filters by the typed link
  /// skill (e.g. `prev_review`). `asOf` hides edges whose source page
  /// was born after the cut; `scenario` filters edges by their source
  /// page's overlay (base only when null).
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
    String? asOf,
    String? scenario,
  });

  /// Catalogue of skills the tenant declares.
  Future<List<SkillSummary>> listSkills();

  /// Instances of [skillId], optionally filtered + ordered. `asOf`
  /// excludes instances born after the cut (untimed always remain).
  /// `scenario` selects a what-if overlay (base ∪ overlay, overlay
  /// wins per slug); null returns the shared base only.
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
    String? scenario,
  });

  // ── events / inbox (M7) ──────────────────────────────────────

  /// Unprocessed events (the inbox), newest first.
  Future<List<Event>> listInbox({int? limit});

  /// An instance's processed event history (the event sequence whose
  /// projection is its state), oldest first.
  Future<List<Event>> listEvents(String instancePageId, {int? limit});

  /// Capture a new event into the inbox. `instancePageId` only
  /// pre-flags a candidate; an external agent assigns + processes it.
  Future<Event> captureEvent({
    String? at,
    String source = '',
    String mime = '',
    String labelSkill = '',
    String? instancePageId,
    String title = '',
    String body = '',
    Map<String, dynamic>? provenance,
  });

  /// Execute a `[[query::*]]` stored query with bound parameters.
  Future<QueryResult> runStoredQuery(String queryId, {Map<String, Object?> params = const {}});

  // ── write primitives ────────────────────────────────────────

  /// Dry-run the indexer's validation pipeline against [content].
  /// Returns the same issue list as [updatePage] but commits nothing.
  Future<ValidationResult> validate(String content, {String? asPageId});

  /// Whole-page write for environments without CRDT op streaming.
  /// Server diffs against current state and applies as ops.
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion});

  // ── live mode (M3+ via WS) ──────────────────────────────────

  /// Open a CRDT session for live, multi-actor editing of [pageId].
  Future<Session> openSession(String pageId);

  /// Apply one CRDT op into an open [session].
  Future<ApplyOpResult> applyOp(String session, CrdtOp op);

  /// Materialise the session's CRDT state to canonical markdown and
  /// release the session.
  Future<CloseResult> closeSession(String session, {bool commit = true});

  /// Subscribe to presence + remote-op awareness for [pageId].
  /// Drains when the underlying socket closes.
  Stream<AwarenessEvent> awareness(String pageId);

  // ── chat history (M-Chat, issue #63) ────────────────────────

  /// Append one message to a chat group's conversation log. The
  /// server stamps `ts`/`msgId` when omitted and returns the
  /// resolved values. `embed = false` skips the embedding cost.
  Future<AppendedMessage> appendMessage({
    required String chatGroupId,
    required String role,
    required String content,
    String? author,
    String? ts,
    Map<String, Object?>? metadata,
    String? msgId,
    bool embed = true,
  });

  /// Read a chat group's history time-ordered. `since` is inclusive,
  /// `until` exclusive; `direction` defaults to newest-first. Pass a
  /// prior page's `nextCursor` to continue.
  Future<ChatPage> listMessages(
    String chatGroupId, {
    String? since,
    String? until,
    int limit = 100,
    String? cursor,
    String direction = 'desc',
  });

  // ── admin MCP tools (require escurel-admin role) ────────────

  /// Per-tenant quota snapshot (`admin_quota`).
  Future<QuotaSnapshot> adminQuota();

  /// Markdown ⟷ DuckDB drift (`admin_audit`).
  Future<AuditDrift> adminAudit();

  /// Purge chat history (`admin_delete_chat_history`). GDPR erasure
  /// (group set), retention prune (beforeTs set), or both. Returns
  /// the number of rows removed.
  Future<int> adminDeleteChatHistory({String? chatGroupId, String? beforeTs});

  /// Enumerate the LaneStores the server has registered.
  Future<List<LaneSummary>> adminListLanes();

  /// List keys under [prefix] in [lane], up to [limit].
  Future<List<LaneKey>> adminLaneKeys(String lane, {String? prefix, int limit = 100});

  /// Fetch one raw blob from [lane] by [key]. Used by the inspector
  /// to show what the LaneStore actually has on disk / in S3.
  Future<LaneBlob> adminLaneBlob(String lane, String key);

  /// Read a range of rows from one of the six index tables
  /// (`pages`, `links`, `blocks`, `crdt_ops`, `crdt_snapshots`,
  /// `frontmatter_index`).
  Future<QueryResult> adminIndexQuery(String table, {Map<String, Object?>? filter, int? limit});

  // ── substrate health ────────────────────────────────────────

  /// `GET /healthz` — dependency-free liveness probe.
  Future<HealthInfo> healthz();

  /// `GET /version` — app, version, git sha, declared capabilities.
  Future<VersionInfo> version();

  // ── lifecycle ───────────────────────────────────────────────

  /// Release any HTTP / WS resources held by the client.
  void close();
}

/// Default [EscurelUnsupportedException] message used by partial
/// implementations to signal a tool isn't wired in yet.
EscurelUnsupportedException notYetImplemented(String tool) =>
    EscurelUnsupportedException('$tool is not implemented by this client');
