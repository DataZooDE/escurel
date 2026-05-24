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
  });

  /// Parse and validate a `[[skill::id]]` reference without
  /// fetching the body. Reports `exists: false` for dangling links
  /// rather than raising.
  Future<ResolveResult> resolve(String wikilink);

  /// Fetch the body (and blocks + outgoing wikilinks) for one page.
  Future<ExpandResult> expand(String pageId, {String? anchor, String? version});

  /// The link-graph primitive: backlinks (`incoming`), forward-links
  /// (`outgoing`), or both. `linkSkill` filters by the typed link
  /// skill (e.g. `prev_review`).
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
  });

  /// Catalogue of skills the tenant declares.
  Future<List<SkillSummary>> listSkills();

  /// Instances of [skillId], optionally filtered + ordered.
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
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

  // ── admin MCP tools (require escurel-admin role) ────────────

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
