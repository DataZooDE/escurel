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
  });

  final String pageId;
  final String skill;
  final PageType pageType;
  final Map<String, dynamic> frontmatter;
  final String body;
  final List<Block> blocks;
  final List<String> wikilinksOut;
  final String? version;
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

class SkillSummary {
  const SkillSummary({
    required this.id,
    required this.description,
    required this.requiredFrontmatter,
    required this.optionalFrontmatter,
    this.isEventTyped = false,
    this.visibility = 'public',
    this.ownerField,
  });

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
  /// owner-bound. Editing is only offered for public/ownerless skills.
  final String visibility;

  /// The frontmatter key naming the owning principal (`owner_field:`),
  /// or null when the skill has no owner binding. A non-null
  /// [ownerField] marks an owner-bound skill (e.g. `private_profile`)
  /// that operators must never edit through the explorer.
  final String? ownerField;
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
}

/// `true` when a page's frontmatter marks it as a tombstone. Shared by
/// instance summaries and expanded pages so the "hide erased" rule is
/// defined in exactly one place.
bool isErasedFrontmatter(Map<String, dynamic> frontmatter) {
  final status = (frontmatter['status'] as String?)?.trim().toLowerCase();
  return status == 'erased' || status == 'revoked';
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
  });

  final List<QueryColumn> columns;
  final List<Map<String, Object?>> rows;
  final String? snapshotVersion;
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
