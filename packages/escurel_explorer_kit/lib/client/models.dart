/// Data-transfer objects for the [EscurelClient] surface.
///
/// Hand-written for v0 — no freezed, no JSON serialisation. JSON
/// marshalling lands with [HttpEscurelClient] in a later PR; until
/// then these are pure in-memory shapes consumed by the fixture
/// client and the editor widgets.
library;

import 'dart:typed_data';

import '../md/frontmatter.dart';

// ── common ──────────────────────────────────────────────────────

/// Restrict a search to pages of a particular kind.
enum PageTypeFilter { skill, instance, any }

/// Block-level vs page-level [search] hits.
enum SearchGranularity { block, page }

/// Direction of [EscurelClient.neighbours] traversal.
enum LinkDirection { incoming, outgoing, both }

/// Backend capabilities a server reports via `/version`. Used by the
/// feature-flag gate to enable/disable editor affordances.
enum BackendCapability {
  /// Always available — pure client-side primitives (md inspector).
  none,

  /// Read-side of the 12-tool agent contract (M2).
  agentReadTools,

  /// Write-side of the 12-tool agent contract (M3).
  agentWriteTools,

  /// `open_session` / `apply_op` / `close_session` over WS (M3).
  liveSession,

  /// `run_stored_query` + admin tools (M4).
  storedQueries,

  /// Admin MCP tools — lane browse, raw index queries (M3, gated by
  /// the `escurel-admin` role).
  adminTools,
}

// ── search ──────────────────────────────────────────────────────

class SearchHit {
  const SearchHit({
    required this.pageId,
    required this.skill,
    required this.score,
    this.anchor,
    this.snippet,
  });

  final String pageId;
  final String skill;
  final double score;
  final String? anchor;
  final String? snippet;
}

class SearchResult {
  const SearchResult({required this.hits, required this.granularity});
  final List<SearchHit> hits;
  final SearchGranularity granularity;
}

// ── resolve ─────────────────────────────────────────────────────

class ResolveResult {
  const ResolveResult({
    required this.pageId,
    required this.skill,
    required this.pageType,
    required this.exists,
    this.description,
    this.error,
  });

  final String pageId;
  final String skill;
  final PageType pageType;
  final bool exists;
  final String? description;
  final String? error;
}

// ── expand ──────────────────────────────────────────────────────

class Block {
  const Block({required this.anchor, required this.content});
  final String anchor;
  final String content;
}

class ExpandResult {
  const ExpandResult({
    required this.pageId,
    required this.skill,
    required this.pageType,
    required this.frontmatter,
    required this.body,
    required this.blocks,
    required this.wikilinksOut,
    this.version,
    this.backendProjection,
    this.chunksTotal,
    this.chunksTruncated = false,
    this.shadow,
  });

  final String pageId;
  final String skill;
  final PageType pageType;
  final Map<String, dynamic> frontmatter;
  final String body;
  final List<Block> blocks;
  final List<String> wikilinksOut;
  final String? version;

  /// For a `sql_view` instance: the bounded projection of the view's rows
  /// rendered beneath the overlay (REQ-SQL-06). `null` for other backends.
  final BackendProjection? backendProjection;

  /// For a `document` instance: the total chunk count (the `blocks` returned
  /// are a bounded lead, REQ-DOC-05). `null` for non-document instances.
  final int? chunksTotal;

  /// Whether the returned `blocks` are a truncated lead of `chunksTotal`.
  final bool chunksTruncated;

  /// When this page is a tenant OVERLAY skill that shadows a pack-imported
  /// base skill of the same id (REQ-LAYER-03): the shadowed base's page id,
  /// pack pin, and frontmatter — so overlay↔base drift stays visible,
  /// never silently masked. `null` for non-shadowing pages and older
  /// servers that don't emit `shadow`.
  final ShadowInfo? shadow;

  /// The `backend_ref` block off the frontmatter, or `null` for native
  /// markdown instances. Tells the UI which backend rendered this instance.
  Map<String, dynamic>? get backendRef =>
      frontmatter['backend_ref'] as Map<String, dynamic>?;

  /// The backend kind (`sql_view` | `document`), or `null` for markdown.
  String? get backendKind => backendRef?['kind'] as String?;
}

/// The shadowed base behind a shadowing overlay skill page (`expand`'s
/// additive `shadow` object, REQ-LAYER-03): the base page's id, the
/// `base@<pack>@<version>` pin it was imported under, and its frontmatter.
/// The overlay wins for display; the base values stay visible so drift is
/// never silently masked.
class ShadowInfo {
  const ShadowInfo({
    required this.basePageId,
    required this.pack,
    required this.base,
  });

  final String basePageId;

  /// The shadowed base's pin, e.g. `base@logistics-midmarket@v7`.
  final String pack;

  /// The shadowed base page's frontmatter.
  final Map<String, dynamic> base;

  factory ShadowInfo.fromJson(Map<String, dynamic> j) => ShadowInfo(
    basePageId: (j['base_page_id'] as String?) ?? '',
    pack: (j['pack'] as String?) ?? '',
    base: Map<String, dynamic>.from(j['base'] as Map? ?? const {}),
  );
}

/// An instance's `backend_projection` from `expand` — one Dart object over
/// two wire families:
///
///  * `sql_view`: the bounded projection of the view's rows, the projected
///    `source.<field>` column map, and an optional `{code, message}` Issue
///    (e.g. `binding_degraded` — reads fail closed on drift).
///  * remote (`openapi`/`mcp`): the LIVE upstream read — the [endpoint]
///    name plus the projected [fields] map on success, or a plain-string
///    issue ([issueMessage] only, no code) on any failure.
class BackendProjection {
  const BackendProjection({
    this.view = '',
    this.rows = const [],
    this.source = const {},
    this.truncated = false,
    this.issueCode,
    this.issueMessage,
    this.endpoint,
    this.fields = const {},
  });

  final String view;
  final List<Map<String, dynamic>> rows;

  /// sql_view: the projected `source.<overlay_field>` column map. Remote
  /// projections reuse the `source` wire key for the endpoint NAME instead
  /// — that lands in [endpoint], and this map stays empty.
  final Map<String, dynamic> source;
  final bool truncated;

  /// sql_view issue code (`binding_degraded`, `source_unavailable`, …).
  /// A remote failure carries no code — only [issueMessage] is set.
  final String? issueCode;
  final String? issueMessage;

  /// Remote (openapi/mcp) only: the registered endpoint the live values
  /// were fetched from (the wire's string-typed `source`).
  final String? endpoint;

  /// Remote (openapi/mcp) only: the projected field→value map
  /// (`{symbol, price, currency, …}`). Empty for sql_view projections.
  final Map<String, dynamic> fields;

  bool get degraded => issueCode != null || issueMessage != null;

  factory BackendProjection.fromJson(Map<String, dynamic> j) {
    // Two wire families share this object. sql_view:
    //   { view, rows, source: {col: val}, truncated?, issue?: {code, message} }
    // remote (openapi/mcp), from remote_backend::fetch_projection:
    //   { source: "<endpoint name>", fields: {…} }  |  { issue: "<message>" }
    // Parse tolerantly: `source` may be a map or a string, `issue` an
    // object or a plain string, and any key may be absent.
    final issue = j['issue'];
    final source = j['source'];
    return BackendProjection(
      view: (j['view'] as String?) ?? '',
      rows: (j['rows'] as List? ?? const [])
          .cast<Map<String, dynamic>>()
          .toList(),
      source: source is Map ? Map<String, dynamic>.from(source) : const {},
      truncated: (j['truncated'] as bool?) ?? false,
      issueCode: issue is Map ? issue['code'] as String? : null,
      issueMessage: switch (issue) {
        final Map m => m['message'] as String?,
        final String s => s,
        _ => null,
      },
      endpoint: source is String ? source : null,
      fields: Map<String, dynamic>.from(j['fields'] as Map? ?? const {}),
    );
  }
}

/// One subscribed skill pack and its pinned version (`list_packs`,
/// REQ-SUB-01): the provenance record behind the tenant's read-only
/// `base@<pack>@<version>` layer.
class PackSubscriptionInfo {
  const PackSubscriptionInfo({
    required this.packId,
    required this.version,
    required this.vertical,
    required this.publisher,
    this.contentHash,
  });

  final String packId;
  final int version;
  final String vertical;
  final String publisher;
  final String? contentHash;

  factory PackSubscriptionInfo.fromJson(Map<String, dynamic> j) =>
      PackSubscriptionInfo(
        packId: (j['pack_id'] as String?) ?? '',
        version: (j['version'] as num?)?.toInt() ?? 0,
        vertical: (j['vertical'] as String?) ?? '',
        publisher: (j['publisher'] as String?) ?? '',
        contentHash: j['content_hash'] as String?,
      );
}

/// Result of a pack lifecycle operation (`import_pack` / `rebase_pack` /
/// `unsubscribe_pack`). Field names mirror the server payloads; absent
/// fields stay null (each tool reports a different subset). A blocked
/// rebase reports `ok: false` plus typed `rebase_conflict` [issues].
class PackOpResult {
  const PackOpResult({
    this.ok = true,
    this.pack = '',
    this.version,
    this.pagesImported,
    this.pagesRemoved,
    this.issues = const [],
  });

  final bool ok;
  final String pack;
  final int? version;
  final int? pagesImported;
  final int? pagesRemoved;

  /// Typed conflicts from a blocked rebase (`rebase_conflict`).
  final List<Issue> issues;

  factory PackOpResult.fromJson(Map<String, dynamic> j) => PackOpResult(
    ok: (j['ok'] as bool?) ?? true,
    pack: (j['pack'] as String?) ?? '',
    version: ((j['version'] ?? j['to_version']) as num?)?.toInt(),
    pagesImported: (j['pages_imported'] as num?)?.toInt(),
    pagesRemoved: (j['pages_removed'] as num?)?.toInt(),
    issues: (j['issues'] as List? ?? const [])
        .whereType<Map>()
        .map(
          (i) => Issue(
            severity: switch (i['severity'] as String?) {
              'warning' => IssueSeverity.warning,
              'info' => IssueSeverity.info,
              _ => IssueSeverity.error,
            },
            code: (i['code'] as String?) ?? '',
            message: (i['message'] as String?) ?? '',
          ),
        )
        .toList(),
  );
}

/// A registered external-source credential — name/connector only, never the
/// secret (`list_credentials`).
class CredentialInfo {
  const CredentialInfo({
    required this.name,
    required this.connector,
    this.createdAt,
    this.createdBy,
  });

  final String name;
  final String connector;
  final String? createdAt;
  final String? createdBy;

  factory CredentialInfo.fromJson(Map<String, dynamic> j) => CredentialInfo(
    name: (j['name'] as String?) ?? '',
    connector: (j['connector'] as String?) ?? '',
    createdAt: j['created_at'] as String?,
    createdBy: j['created_by'] as String?,
  );
}

/// Health of one SQL-view binding from `validate_bindings`: `ok` |
/// `binding_degraded` | `backend_unavailable`.
class BindingStatus {
  const BindingStatus({
    required this.pageId,
    required this.view,
    required this.status,
    this.detail,
  });

  final String pageId;
  final String view;
  final String status;
  final String? detail;

  bool get healthy => status == 'ok';

  factory BindingStatus.fromJson(Map<String, dynamic> j) => BindingStatus(
    pageId: (j['page_id'] as String?) ?? '',
    view: (j['view'] as String?) ?? '',
    status: (j['status'] as String?) ?? 'unknown',
    detail: j['detail'] as String?,
  );
}

/// One admin-registered remote-backend endpoint — base URL + auth scheme,
/// never the secret (`list_endpoints`, REQ-REMOTE-05).
class EndpointInfo {
  const EndpointInfo({
    required this.name,
    required this.kind,
    required this.baseUrl,
    this.authScheme = 'none',
    this.createdAt,
    this.createdBy,
  });

  final String name;

  /// `openapi` | `mcp`.
  final String kind;
  final String baseUrl;

  /// `none` | `bearer` | `api_key`.
  final String authScheme;
  final String? createdAt;
  final String? createdBy;

  factory EndpointInfo.fromJson(Map<String, dynamic> j) => EndpointInfo(
    name: (j['name'] as String?) ?? '',
    kind: (j['kind'] as String?) ?? '',
    baseUrl: (j['base_url'] as String?) ?? '',
    authScheme: (j['auth_scheme'] as String?) ?? 'none',
    createdAt: j['created_at'] as String?,
    createdBy: j['created_by'] as String?,
  );
}

/// Reachability of one registered endpoint from `validate_endpoints`:
/// `ok` | `unreachable` (+ a transport detail when unreachable).
class EndpointHealth {
  const EndpointHealth({
    required this.name,
    required this.kind,
    required this.status,
    this.detail,
  });

  final String name;
  final String kind;
  final String status;
  final String? detail;

  bool get healthy => status == 'ok';

  factory EndpointHealth.fromJson(Map<String, dynamic> j) => EndpointHealth(
    name: (j['name'] as String?) ?? '',
    kind: (j['kind'] as String?) ?? '',
    status: (j['status'] as String?) ?? 'unknown',
    detail: j['detail'] as String?,
  );
}

/// The outcome of a document ingestion (`/ingest` or `/ingest/upload`):
/// `materialised` | `extraction_failed` | `no_handler` | `queued`.
class IngestOutcome {
  const IngestOutcome({
    required this.status,
    this.eventId,
    this.pageId,
    this.handlerSkill,
    this.chunkCount,
    this.issueCode,
    this.issueMessage,
  });

  final String status;
  final String? eventId;
  final String? pageId;
  final String? handlerSkill;
  final int? chunkCount;
  final String? issueCode;
  final String? issueMessage;

  bool get materialised => status == 'materialised';

  factory IngestOutcome.fromJson(Map<String, dynamic> j) {
    final issue = j['issue'] as Map<String, dynamic>?;
    return IngestOutcome(
      status: (j['status'] as String?) ?? 'unknown',
      eventId: j['event_id'] as String?,
      pageId: j['page_id'] as String?,
      handlerSkill: j['handler_skill'] as String?,
      chunkCount: (j['chunk_count'] as num?)?.toInt(),
      issueCode: issue?['code'] as String?,
      issueMessage: issue?['message'] as String?,
    );
  }
}

// ── neighbours ──────────────────────────────────────────────────

class Neighbour {
  const Neighbour({
    required this.src,
    required this.dst,
    required this.linkSkill,
    this.anchor,
    this.linkVersion,
  });

  final String src;
  final String dst;
  final String linkSkill;
  final String? anchor;
  final String? linkVersion;
}

// ── list_skills / list_instances ────────────────────────────────

/// The per-CRUD group ACL a skill declares (group ACL v1), mirroring the
/// server's `acl` object on `list_skills`. Each verb is a list of group
/// names (`public`/`owner`/`admin` reserved, others custom); a verb left
/// `null` is *omitted* and falls through to the tenant default. `null` for
/// the whole [SkillAcl] means the skill declared neither an `acl:` block
/// nor a legacy `visibility:` field.
class SkillAcl {
  const SkillAcl({this.read, this.create, this.update, this.delete});

  final List<String>? read;
  final List<String>? create;
  final List<String>? update;
  final List<String>? delete;

  /// Parse from the server's `acl` JSON object, or `null` when absent.
  static SkillAcl? fromJson(Object? raw) {
    if (raw is! Map) return null;
    List<String>? verb(String k) => raw[k] == null
        ? null
        : (raw[k] as List).map((e) => e.toString()).toList();
    return SkillAcl(
      read: verb('read'),
      create: verb('create'),
      update: verb('update'),
      delete: verb('delete'),
    );
  }
}

/// What a skill's backend can do (from `list_skills`). Drives read-only-ness
/// + how an instance is rendered. Absent on older servers → markdown defaults.
class SkillCapabilities {
  const SkillCapabilities({
    this.writable = true,
    this.granularity = 'block',
    this.search = 'hybrid',
    this.supportsCrdt = true,
  });

  final bool writable;
  final String granularity;
  final String search;
  final bool supportsCrdt;

  factory SkillCapabilities.fromJson(Map<String, dynamic> j) =>
      SkillCapabilities(
        writable: (j['writable'] as bool?) ?? true,
        granularity: (j['granularity'] as String?) ?? 'block',
        search: (j['search'] as String?) ?? 'hybrid',
        supportsCrdt: (j['supports_crdt'] as bool?) ?? true,
      );
}

class SkillSummary {
  const SkillSummary({
    required this.id,
    required this.description,
    required this.requiredFrontmatter,
    required this.optionalFrontmatter,
    this.isEventTyped = false,
    this.visibility = 'public',
    this.ownerField,
    this.acl,
    this.backendKind = 'markdown',
    this.capabilities = const SkillCapabilities(),
    this.layer = 'overlay',
    this.shadows,
  });

  /// The backend a skill's instances live in (`markdown` | `sql_view` |
  /// `document`); `markdown` when the skill declares no `backend:` block.
  final String backendKind;

  /// The backend's capability descriptor (read-only-ness, granularity, …).
  final SkillCapabilities capabilities;

  final String id;
  final String description;
  final List<String> requiredFrontmatter;
  final List<String> optionalFrontmatter;

  /// Whether the skill is event-typed (its instances are dated events,
  /// e.g. `email` / `meeting` / `doc`) vs entity-bound (`customer`,
  /// `contact`, …). Drives the skills-registry grouping.
  final bool isEventTyped;

  /// The skill's instance-ACL visibility class as declared in its
  /// frontmatter (`visibility:`). `"public"` (the default when absent)
  /// means anyone may read; `"owner"` (and similar) means access is
  /// owner-bound. Retained as a derived convenience; [acl] is the full
  /// model.
  final String visibility;

  /// The frontmatter key naming the owning principal (`owner_field:`),
  /// or null when the skill has no owner binding. A non-null
  /// [ownerField] marks an owner-bound skill (e.g. `private_profile`)
  /// that operators must never edit through the explorer.
  final String? ownerField;

  /// The resolved per-CRUD group ACL (group ACL v1), or `null` when the
  /// skill declares neither an `acl:` block nor a legacy `visibility:`
  /// field.
  final SkillAcl? acl;

  /// The skill page's stability layer (REQ-LAYER-04): `'overlay'` for a
  /// tenant-authored (editable) skill — the default, and what older
  /// servers that don't emit the field map to — or
  /// `'base@<pack>@<version>'` for a skill imported from a subscribed
  /// pack, read-only at this node.
  final String layer;

  /// When this overlay skill shadows a pack-imported base skill of the
  /// same id (REQ-LAYER-03): the shadowed base's `base@<pack>@<version>`
  /// pin, else null. The catalogue shows one entry per skill id — the
  /// overlay — carrying this pin.
  final String? shadows;

  /// Whether this skill was imported from a subscribed pack (its layer
  /// is a `base@<pack>@<version>` pin). Base skills are read-only: the
  /// server rejects writes with `layer_read_only`.
  bool get isBaseLayer => layer.startsWith('base@');

  /// Whether this skill is offered as operator-editable in the explorer.
  ///
  /// This is a UX guardrail (the backend still enforces auth), generalised
  /// from the legacy "ownerless ⇒ editable" rule to the group model: a
  /// skill is editable iff its `update` policy is **not owner-scoped** —
  /// i.e. some non-`owner` principal (admin / a custom group) may update
  /// it, which the operator dashboard can. When the skill declares no
  /// explicit `update` policy, fall back to the legacy ownerless check.
  ///
  /// Deliberately layer-blind: this getter governs a skill's INSTANCES,
  /// and a tenant specialises a base skill precisely by authoring overlay
  /// instances. Only the base PAGE itself is read-only — that is gated
  /// per-page in `currentPageEditableProvider` (agy review MUST-FIX 2).
  bool get operatorEditable {
    final update = acl?.update;
    if (update == null) return ownerField == null;
    return !update.contains('owner');
  }
}

class InstanceSummary {
  const InstanceSummary({
    required this.id,
    required this.skill,
    required this.frontmatter,
  });

  final String id;
  final String skill;
  final Map<String, dynamic> frontmatter;

  /// Whether this instance is a tombstone (erased/revoked on user
  /// request). Carl's `erase_member` writes `status: erased` on the member
  /// and `status: revoked` on its consent; both keep their required keys so
  /// the page still parses. Treated as a deletion marker the explorer hides
  /// by default.
  bool get erased => isErasedFrontmatter(frontmatter);

  /// Whether this instance is archived. Unlike [erased] (a permanent
  /// tombstone), archiving is a recoverable "put away" state: herkules writes
  /// `archived: true` in the instance frontmatter. The catalogue hides archived
  /// rows per skill by default, with a toggle to reveal them.
  bool get archived => isArchivedFrontmatter(frontmatter);
}

/// `true` when a page's frontmatter marks it as a tombstone. Shared by
/// instance summaries and expanded pages so the "hide erased" rule is
/// defined in exactly one place.
bool isErasedFrontmatter(Map<String, dynamic> frontmatter) {
  final status = (frontmatter['status'] as String?)?.trim().toLowerCase();
  return status == 'erased' || status == 'revoked';
}

/// `true` when a page's frontmatter marks it archived (`archived: true`).
/// Tolerates the value arriving as a real bool (native/HTTP backend YAML→JSON)
/// or the string `"true"`. One definition shared by every "hide archived" rule.
bool isArchivedFrontmatter(Map<String, dynamic> frontmatter) {
  final v = frontmatter['archived'];
  if (v is bool) return v;
  return v is String && v.trim().toLowerCase() == 'true';
}

// ── events / inbox (M7) ─────────────────────────────────────────

/// One event — the dynamic input of the memory triad. Its [labelSkill]
/// links to the skill that knows how to process it; [instancePageId] is
/// the instance it belongs to once processed.
class Event {
  const Event({
    required this.eventId,
    required this.at,
    required this.source,
    required this.mime,
    required this.labelSkill,
    required this.instancePageId,
    required this.status,
    required this.title,
    required this.body,
    required this.provenance,
  });

  final String eventId;
  final String? at;
  final String source;
  final String mime;
  final String labelSkill;
  final String? instancePageId;
  final String status;
  final String title;
  final String body;
  final Map<String, dynamic> provenance;

  bool get isInbox => status == 'inbox';

  static Event fromJson(Map<String, dynamic> j) => Event(
    eventId: (j['event_id'] as String?) ?? '',
    at: j['at'] as String?,
    source: (j['source'] as String?) ?? '',
    mime: (j['mime'] as String?) ?? '',
    labelSkill: (j['label_skill'] as String?) ?? '',
    instancePageId: j['instance_page_id'] as String?,
    status: (j['status'] as String?) ?? 'inbox',
    title: (j['title'] as String?) ?? '',
    body: (j['body'] as String?) ?? '',
    provenance: Map<String, dynamic>.from(j['provenance'] as Map? ?? const {}),
  );
}

// ── run_stored_query ────────────────────────────────────────────

class QueryColumn {
  const QueryColumn({required this.name, required this.dartType});
  final String name;
  final String dartType;
}

class QueryResult {
  const QueryResult({
    required this.columns,
    required this.rows,
    this.snapshotVersion,
    this.truncated = false,
  });

  final List<QueryColumn> columns;
  final List<Map<String, Object?>> rows;
  final String? snapshotVersion;

  /// `query_instance` only: whether the server capped the result set
  /// (the tool's additive `truncated` flag). Always false elsewhere.
  final bool truncated;
}

// ── validate / update_page ──────────────────────────────────────

enum IssueSeverity { error, warning, info }

class Issue {
  const Issue({
    required this.severity,
    required this.code,
    required this.message,
    this.line,
    this.column,
  });

  final IssueSeverity severity;
  final String code;
  final String message;
  final int? line;
  final int? column;
}

class ValidationResult {
  const ValidationResult({required this.issues});
  final List<Issue> issues;

  bool get isOk => issues.every((i) => i.severity != IssueSeverity.error);
}

class UpdateResult {
  const UpdateResult({required this.ok, required this.issues, this.newVersion});
  final bool ok;
  final List<Issue> issues;
  final String? newVersion;
}

// ── live mode (session) — stubs until M3 transport decided ──────

class Session {
  const Session({
    required this.id,
    required this.pageId,
    required this.headVersion,
    required this.content,
  });
  final String id;
  final String pageId;
  final String headVersion;
  final String content;
}

class CrdtOp {
  const CrdtOp({required this.kind, required this.payload});
  final String kind;
  final Map<String, Object?> payload;
}

class ApplyOpResult {
  const ApplyOpResult({required this.ok, this.conflicts});
  final bool ok;
  final List<Map<String, Object?>>? conflicts;
}

class CloseResult {
  const CloseResult({required this.finalVersion, required this.issues});
  final String finalVersion;
  final List<Issue> issues;
}

class AwarenessEvent {
  const AwarenessEvent({
    required this.session,
    required this.kind,
    this.payload,
  });
  final String session;
  final String kind;
  final Map<String, Object?>? payload;
}

// ── admin MCP tools — gated by escurel-admin role ───────────────

class LaneSummary {
  const LaneSummary({
    required this.name,
    required this.backend,
    required this.tenantsPresent,
  });
  final String name;
  final String backend;
  final int tenantsPresent;
}

class LaneKey {
  const LaneKey({
    required this.key,
    required this.sizeBytes,
    this.lastModified,
  });
  final String key;
  final int sizeBytes;
  final DateTime? lastModified;
}

class LaneBlob {
  const LaneBlob({
    required this.key,
    required this.bytes,
    required this.contentType,
  });
  final String key;
  final Uint8List bytes;
  final String contentType;
}

// ── substrate health ────────────────────────────────────────────

class HealthInfo {
  const HealthInfo({required this.ok, required this.checkedAt});
  final bool ok;
  final DateTime checkedAt;
}

class VersionInfo {
  const VersionInfo({
    required this.app,
    required this.version,
    required this.gitSha,
    required this.capabilities,
  });

  final String app;
  final String version;
  final String gitSha;
  final Set<BackendCapability> capabilities;
}

// ── chat history (M-Chat, issue #63) ────────────────────────────

/// One message in a per-chat-group conversation log. Distinct from
/// the typed-instance KB — an append-mostly row keyed by an opaque
/// `chatGroupId` (the consumer owns the id scheme).
class ChatMessage {
  const ChatMessage({
    required this.chatGroupId,
    required this.msgId,
    required this.ts,
    required this.role,
    required this.content,
    required this.embedded,
    this.author,
    this.metadata,
  });

  final String chatGroupId;
  final String msgId;

  /// RFC-3339 UTC, e.g. `2026-05-26T10:00:00Z`.
  final String ts;

  /// `user` | `assistant` | `system` | `tool`.
  final String role;
  final String content;

  /// Whether this row carries a dense embedding (server `embed` flag).
  final bool embedded;
  final String? author;
  final Map<String, Object?>? metadata;
}

/// One page of [ChatMessage]s plus an opaque cursor for the next page
/// (`null` when the history is exhausted).
class ChatPage {
  const ChatPage({required this.messages, this.nextCursor});
  final List<ChatMessage> messages;
  final String? nextCursor;
}

/// Result of `append_message` — the resolved id + timestamp the
/// server persisted (it stamps both when the caller omits them).
class AppendedMessage {
  const AppendedMessage({required this.msgId, required this.ts});
  final String msgId;
  final String ts;
}

// ── RBAC group membership (admin tools) ─────────────────────────

/// One subject's membership in an RBAC group (`list_group_members`).
class GroupMember {
  const GroupMember({
    required this.groupId,
    required this.subject,
    this.addedAt,
    this.addedBy,
  });

  final String groupId;
  final String subject;
  final String? addedAt;
  final String? addedBy;
}

// ── admin ops tools (escurel-admin role) ────────────────────────

/// Per-tenant rate / concurrency budget snapshot (`admin_quota`).
class QuotaSnapshot {
  const QuotaSnapshot({
    required this.queriesRemaining,
    required this.writesRemaining,
    required this.embedsRemaining,
    required this.concurrentSessionsInUse,
  });

  final int queriesRemaining;
  final int writesRemaining;
  final int embedsRemaining;
  final int concurrentSessionsInUse;
}

/// One outbound webhook callback attempt (`admin_webhook_deliveries`).
class WebhookDelivery {
  const WebhookDelivery({
    required this.eventId,
    required this.atMs,
    required this.ok,
    this.httpStatus,
    this.error,
  });

  final String eventId;
  final int atMs;
  final bool ok;
  final int? httpStatus;
  final String? error;
}

/// The outbound webhook delivery log (`admin_webhook_deliveries`),
/// newest first. `configured` is false when no webhook sink is set,
/// in which case [deliveries] is empty.
class WebhookDeliveries {
  const WebhookDeliveries({required this.configured, required this.deliveries});

  final bool configured;
  final List<WebhookDelivery> deliveries;
}

/// Drift between canonical markdown and the DuckDB index
/// (`admin_audit`).
class AuditDrift {
  const AuditDrift({
    required this.markdownNotInDuckdb,
    required this.indexedButNoMarkdown,
  });

  final List<String> markdownNotInDuckdb;
  final List<String> indexedButNoMarkdown;

  bool get isClean =>
      markdownNotInDuckdb.isEmpty && indexedButNoMarkdown.isEmpty;
}
