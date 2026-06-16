/// In-memory escurel client backed by a hand-supplied page corpus.
///
/// Implements the read side of [EscurelClient] over parsed
/// `examples/crm-demo/`-style markdown — frontmatter, body,
/// wikilinks — without touching the network. Used by:
///
/// - The explorer's `--mode=fixture` build for offline demo
///   (`dz-escurel-explore-fixture` Nomad job).
/// - The merge-gate integration test (PR-4) — boots the editor
///   under headless chrome against this client and asserts the
///   render path works end-to-end.
/// - Unit tests of widgets that consume [EscurelClient].
///
/// Write tools, live mode, admin tools, and stored queries all
/// surface as [EscurelUnsupportedException] — the corpus is
/// read-only and not a real server.
library;

import '../md/frontmatter.dart' as md;
import '../md/wikilink.dart';
import 'errors.dart';
import 'escurel_client.dart';
import 'models.dart';

/// One parsed page indexed by id, with its outgoing wikilinks
/// pre-extracted for `neighbours` / `expand`.
class _ParsedPage {
  _ParsedPage({
    required this.id,
    required this.skill,
    required this.pageType,
    required this.frontmatter,
    required this.body,
    required this.wikilinksOut,
    this.writeSeq = 0,
  });

  final String id;
  final String skill;
  final md.PageType pageType;
  final Map<String, dynamic> frontmatter;
  final String body;
  final List<WikilinkRef> wikilinksOut;

  /// The write-counter value at which this page was last written (0 for
  /// seeded pages). Surfaced as the page version so the editor can pass
  /// it back as `baseVersion` for optimistic concurrency.
  final int writeSeq;
}

class FixtureEscurelClient implements EscurelClient {
  /// Build from a map of `<skill>__<id>` → raw markdown. Parses
  /// each page eagerly; throws [EscurelToolException] if any
  /// file fails to parse so the user discovers seed problems at
  /// startup rather than mid-render.
  ///
  /// The map key encodes the page id and skill via the convention
  /// already used in `examples/crm-demo/instances/`:
  /// `<skill>__<id>.md` → instance, `<skill>.md` → the skill itself.
  factory FixtureEscurelClient.fromSources({
    required Map<String, String> skillFiles,
    required Map<String, String> instanceFiles,
    List<Event> events = const [],
    Map<String, List<String>> snapshots = const {},
    bool writeEnabled = false,
  }) {
    final pages = <String, _ParsedPage>{};

    skillFiles.forEach((basename, raw) {
      final parsed = _tryParse(basename, raw);
      if (parsed.frontmatter.pageType != md.PageType.skill) {
        throw EscurelToolException(
          'expected type: skill in $basename',
          code: 'fixture.wrong_type',
        );
      }
      final id = parsed.frontmatter.fields['id'] as String? ?? basename;
      pages[id] = _ParsedPage(
        id: id,
        skill: id,
        pageType: md.PageType.skill,
        frontmatter: parsed.frontmatter.fields,
        body: parsed.body,
        wikilinksOut: parseWikilinks(parsed.body),
      );
    });

    instanceFiles.forEach((basename, raw) {
      final parsed = _tryParse(basename, raw);
      if (parsed.frontmatter.pageType != md.PageType.instance) {
        throw EscurelToolException(
          'expected type: instance in $basename',
          code: 'fixture.wrong_type',
        );
      }
      final fields = parsed.frontmatter.fields;
      final skill = fields['skill'] as String? ??
          (throw EscurelToolException('missing skill: in $basename', code: 'fixture.missing_skill'));
      final id = fields['id'] as String? ??
          (throw EscurelToolException('missing id: in $basename', code: 'fixture.missing_id'));
      final qualifiedId = '${skill}__$id';
      pages[qualifiedId] = _ParsedPage(
        id: qualifiedId,
        skill: skill,
        pageType: md.PageType.instance,
        frontmatter: fields,
        body: parsed.body,
        wikilinksOut: _outgoingFromInstance(fields, parsed.body),
      );
    });

    return FixtureEscurelClient._(pages, [...events], snapshots, writeEnabled);
  }

  FixtureEscurelClient._(this._pages, this._events, this._snapshots, this._writeEnabled);

  final Map<String, _ParsedPage> _pages;

  /// Whether the write path (`validate`/`update_page`) is live and
  /// `version()` advertises [BackendCapability.agentWriteTools]. Default
  /// false keeps read-only fixtures backward-compatible; the editing
  /// tests opt in with `writeEnabled: true`.
  final bool _writeEnabled;

  /// Monotonic version counter bumped on every successful write — the
  /// fixture's stand-in for the server's CRDT head version.
  int _writeSeq = 0;

  /// Events keyed by their (short) instance page id; processed events
  /// form an instance's history, `inbox` events the global inbox.
  /// Mutable so [captureEvent] can append.
  final List<Event> _events;

  /// Snapshot `taken_at` timestamps per (short) page id — the
  /// state-over-time version markers.
  final Map<String, List<String>> _snapshots;

  static md.Page _tryParse(String basename, String raw) {
    try {
      return md.parse(raw);
    } on md.ParseException catch (e) {
      throw EscurelToolException('parse $basename: ${e.message}', code: 'fixture.parse_failed');
    }
  }

  /// Frontmatter on an instance can hold wikilinks in field values
  /// (e.g. `customer: [[customer::muenchner-pharma]]`). YAML parses
  /// the unquoted `[[…]]` form as a nested flow sequence, so the
  /// recovered value is a `List<List<String>>` whose `toString()`
  /// reconstructs the wikilink syntax. Walk both the body and the
  /// frontmatter (recursively) so the link graph is complete
  /// whichever shape the YAML parser returns.
  static List<WikilinkRef> _outgoingFromInstance(
    Map<String, dynamic> fields,
    String body,
  ) {
    final refs = <WikilinkRef>[...parseWikilinks(body)];
    void walk(dynamic v) {
      if (v == null) return;
      if (v is Map) {
        v.values.forEach(walk);
        return;
      }
      // For strings and lists alike, toString() preserves the
      // [[skill::id]] markup. The regex requires both brackets so
      // unrelated values (numbers, dates) produce no false hits.
      refs.addAll(parseWikilinks(v.toString()));
    }

    fields.values.forEach(walk);
    return refs;
  }

  // ── read tools ──────────────────────────────────────────────

  @override
  Future<List<SkillSummary>> listSkills() async {
    return _pages.values.where((p) => p.pageType == md.PageType.skill).map((p) {
      final required = _stringList(p.frontmatter['required_frontmatter']);
      return SkillSummary(
        id: p.id,
        description: (p.frontmatter['description'] as String?) ?? '',
        requiredFrontmatter: required,
        optionalFrontmatter: _stringList(p.frontmatter['optional_frontmatter']),
        // Mirror the backend: event-typed iff `required_frontmatter`
        // includes `at` (read.rs).
        isEventTyped: required.contains('at'),
        // Instance-ACL hints from the skill frontmatter; default to
        // public/ownerless when absent (→ operator-editable).
        visibility: (p.frontmatter['visibility'] as String?) ?? 'public',
        ownerField: p.frontmatter['owner_field'] as String?,
      );
    }).toList();
  }

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf, // ignored in fixture mode; honoured by the HTTP backend
    String? scenario, // ignored in fixture mode; honoured by the HTTP backend
  }) async {
    var instances = _pages.values
        .where((p) => p.pageType == md.PageType.instance && p.skill == skillId)
        .toList();

    if (filter != null) {
      instances = instances.where((p) => filter.entries.every((e) {
        final actual = p.frontmatter[e.key];
        return _matches(actual, e.value);
      })).toList();
    }

    if (orderBy != null) {
      final field = orderBy.split(' ').first;
      final descending = orderBy.endsWith(' desc');
      instances.sort((a, b) {
        final av = a.frontmatter[field]?.toString() ?? '';
        final bv = b.frontmatter[field]?.toString() ?? '';
        return descending ? bv.compareTo(av) : av.compareTo(bv);
      });
    }

    if (limit != null && instances.length > limit) {
      instances = instances.sublist(0, limit);
    }

    return instances
        .map((p) => InstanceSummary(id: p.id, skill: p.skill, frontmatter: p.frontmatter))
        .toList();
  }

  @override
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) async {
    final refs = parseWikilinks(wikilink);
    if (refs.isEmpty || refs.first.id == null) {
      return const ResolveResult(
        pageId: '',
        skill: '',
        pageType: md.PageType.instance,
        exists: false,
        error: 'malformed wikilink',
      );
    }
    final ref = refs.first;
    final candidates = _pages.values.where((p) {
      if (ref.skill != null) return p.skill == ref.skill && p.id.endsWith('__${ref.id}');
      return p.id == ref.id || p.id.endsWith('__${ref.id}');
    });

    if (candidates.isEmpty) {
      return ResolveResult(
        pageId: ref.skill != null ? '${ref.skill}__${ref.id}' : ref.id!,
        skill: ref.skill ?? '',
        pageType: md.PageType.instance,
        exists: false,
      );
    }

    final p = candidates.first;
    return ResolveResult(
      pageId: p.id,
      skill: p.skill,
      pageType: p.pageType,
      exists: true,
      description: p.frontmatter['description'] as String?,
    );
  }

  @override
  Future<ExpandResult> expand(String pageId,
      {String? anchor, String? version, String? asOf, String? scenario}) async {
    final p = _pages[pageId] ?? (throw EscurelToolException(
      'page $pageId not found',
      code: 'fixture.no_such_page',
    ));
    return ExpandResult(
      pageId: p.id,
      skill: p.skill,
      pageType: p.pageType,
      frontmatter: p.frontmatter,
      body: p.body,
      blocks: const <Block>[],
      wikilinksOut: p.wikilinksOut.map((r) => r.toMarkup()).toList(),
      // Surface a version once a page has been written so the editor can
      // round-trip it as `baseVersion`. Seeded pages report none.
      version: p.writeSeq == 0 ? null : 'fx-${p.writeSeq}',
    );
  }

  @override
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
    String? asOf, // ignored in fixture mode; honoured by the HTTP backend
    String? scenario, // ignored in fixture mode; honoured by the HTTP backend
  }) async {
    final out = <Neighbour>[];
    // De-dupe edges the way the backend's `links` PK does (a field link
    // and a body link for the same edge collapse to one).
    final seen = <String>{};
    void add(String src, String dst, String skill) {
      if (seen.add('$src|$dst|$skill')) {
        out.add(Neighbour(src: src, dst: dst, linkSkill: skill));
      }
    }

    if (direction == LinkDirection.outgoing || direction == LinkDirection.both) {
      final p = _pages[pageId];
      if (p != null) {
        for (final ref in p.wikilinksOut) {
          if (_resolveRef(ref) == null) continue; // skip dangling
          if (linkSkill != null && ref.skill != linkSkill) continue;
          // Match the backend's wire shape: `dst` is the link's slug.
          add(p.id, ref.id ?? '', ref.skill ?? '');
        }
      }
    }

    if (direction == LinkDirection.incoming || direction == LinkDirection.both) {
      for (final cand in _pages.values) {
        for (final ref in cand.wikilinksOut) {
          if (_resolveRef(ref) != pageId) continue;
          if (linkSkill != null && ref.skill != linkSkill) continue;
          add(cand.id, ref.id ?? '', ref.skill ?? '');
        }
      }
    }

    return out;
  }

  @override
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
    String? asOf, // ignored in fixture mode; honoured by the HTTP backend
  }) async {
    // Fixture search: case-insensitive substring over title + body
    // + skill name. Good enough for offline demo; the real ranking
    // arrives with M2.
    final needle = q.toLowerCase();
    final hits = <SearchHit>[];
    for (final p in _pages.values) {
      if (pageType == PageTypeFilter.skill && p.pageType != md.PageType.skill) continue;
      if (pageType == PageTypeFilter.instance && p.pageType != md.PageType.instance) continue;
      if (skill != null && p.skill != skill) continue;
      final inBody = p.body.toLowerCase().contains(needle);
      final inSkill = p.skill.toLowerCase().contains(needle);
      final inId = p.id.toLowerCase().contains(needle);
      if (!inBody && !inSkill && !inId) continue;
      hits.add(SearchHit(
        pageId: p.id,
        skill: p.skill,
        score: inSkill ? 1.0 : (inId ? 0.8 : 0.5),
      ));
    }
    hits.sort((a, b) => b.score.compareTo(a.score));
    return SearchResult(
      hits: hits.take(k).toList(),
      granularity: granularity,
    );
  }

  // ── events / inbox / snapshots (real, over the fixture corpus) ──

  @override
  Future<List<Event>> listInbox({int? limit}) async {
    final inbox = _events.where((e) => e.status == 'inbox').toList();
    return (limit != null && inbox.length > limit) ? inbox.sublist(0, limit) : inbox;
  }

  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async {
    final hist = _events
        .where((e) => e.status == 'processed' && e.instancePageId == instancePageId)
        .toList();
    return (limit != null && hist.length > limit) ? hist.sublist(0, limit) : hist;
  }

  @override
  Future<List<String>> listSnapshots(String pageId) async => _snapshots[pageId] ?? const [];

  @override
  Future<Event> captureEvent({
    String? at,
    String source = '',
    String mime = '',
    String labelSkill = '',
    String? instancePageId,
    String title = '',
    String body = '',
    Map<String, dynamic>? provenance,
  }) async {
    final event = Event(
      eventId: 'fixture-ev-${_events.length}',
      at: at,
      source: source,
      mime: mime,
      labelSkill: labelSkill,
      instancePageId: instancePageId,
      status: 'inbox',
      title: title,
      body: body,
      provenance: provenance ?? const {},
    );
    _events.add(event);
    return event;
  }

  @override
  Future<QueryResult> runStoredQuery(String queryId, {Map<String, Object?> params = const {}}) async =>
      throw notYetImplemented('run_stored_query');

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async {
    if (!_writeEnabled) throw notYetImplemented('validate');
    return ValidationResult(issues: _validateContent(content));
  }

  @override
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion}) async {
    if (!_writeEnabled) throw notYetImplemented('update_page');

    // Mirror the server's optimistic-concurrency gate: a stale
    // baseVersion is rejected without mutating the corpus.
    if (baseVersion != null) {
      final existing = _pages[pageId];
      final head = existing == null ? null : 'fx-${existing.writeSeq}';
      if (head != null && baseVersion != head) {
        return UpdateResult(
          ok: false,
          issues: [
            Issue(
              severity: IssueSeverity.error,
              code: 'stale_base_version',
              message: 'baseVersion $baseVersion does not match head $head',
            ),
          ],
        );
      }
    }

    final issues = _validateContent(content);
    if (issues.any((i) => i.severity == IssueSeverity.error)) {
      return UpdateResult(ok: false, issues: issues);
    }

    final md.Page parsed;
    try {
      parsed = md.parse(content);
    } on md.ParseException catch (e) {
      return UpdateResult(
        ok: false,
        issues: [Issue(severity: IssueSeverity.error, code: 'parse_failed', message: e.message)],
      );
    }
    final fields = parsed.frontmatter.fields;
    final version = 'fx-${++_writeSeq}';

    if (parsed.frontmatter.pageType == md.PageType.skill) {
      final id = (fields['id'] as String?) ?? pageId;
      _pages[id] = _ParsedPage(
        id: id,
        skill: id,
        pageType: md.PageType.skill,
        frontmatter: fields,
        body: parsed.body,
        wikilinksOut: parseWikilinks(parsed.body),
        writeSeq: _writeSeq,
      );
      return UpdateResult(ok: true, issues: issues, newVersion: version);
    }

    final skill = fields['skill'] as String? ?? '';
    final id = fields['id'] as String? ?? '';
    // Key the page the way fromSources does so list/expand/resolve all
    // see the write — `<skill>__<id>`, not the on-disk path.
    final qualifiedId = '${skill}__$id';
    _pages[qualifiedId] = _ParsedPage(
      id: qualifiedId,
      skill: skill,
      pageType: md.PageType.instance,
      frontmatter: fields,
      body: parsed.body,
      wikilinksOut: _outgoingFromInstance(fields, parsed.body),
      writeSeq: _writeSeq,
    );
    return UpdateResult(ok: true, issues: issues, newVersion: version);
  }

  /// A small-but-real validation pass mirroring the indexer's hard
  /// gates: the content must parse as frontmatter+body and carry the
  /// structural keys (`type`, `id`, plus `skill` for instances). Returns
  /// an `error` Issue per missing key; an empty list when the page is ok.
  List<Issue> _validateContent(String content) {
    final md.Page parsed;
    try {
      parsed = md.parse(content);
    } on md.ParseException catch (e) {
      return [Issue(severity: IssueSeverity.error, code: 'parse_failed', message: e.message)];
    }
    final fields = parsed.frontmatter.fields;
    final issues = <Issue>[];
    void requireKey(String key) {
      final v = fields[key];
      if (v == null || (v is String && v.trim().isEmpty)) {
        issues.add(Issue(
          severity: IssueSeverity.error,
          code: 'missing_required',
          message: 'frontmatter is missing required key "$key"',
        ));
      }
    }

    requireKey('type');
    requireKey('id');
    if (parsed.frontmatter.pageType == md.PageType.instance) {
      requireKey('skill');
      // Enforce the instance's skill-declared required frontmatter — a
      // real gate the indexer applies, and what makes "clear a required
      // field → error" a genuine validation path through the form.
      final skillId = fields['skill'] as String?;
      final skillPage = skillId == null ? null : _pages[skillId];
      if (skillPage != null) {
        for (final k in _stringList(skillPage.frontmatter['required_frontmatter'])) {
          if (k == 'id') continue; // structural, already checked
          requireKey(k);
        }
      }
    }
    return issues;
  }

  @override
  Future<Session> openSession(String pageId) async => throw notYetImplemented('open_session');

  @override
  Future<ApplyOpResult> applyOp(String session, CrdtOp op) async =>
      throw notYetImplemented('apply_op');

  @override
  Future<CloseResult> closeSession(String session, {bool commit = true}) async =>
      throw notYetImplemented('close_session');

  @override
  Stream<AwarenessEvent> awareness(String pageId) async* {
    throw notYetImplemented('awareness');
  }

  // ── chat history (in-memory; offline-demo parity) ───────────

  final Map<String, List<ChatMessage>> _chat = {};
  int _chatSeq = 0;

  @override
  Future<AppendedMessage> appendMessage({
    required String chatGroupId,
    required String role,
    required String content,
    String? author,
    String? ts,
    Map<String, Object?>? metadata,
    String? msgId,
    bool embed = true,
  }) async {
    final id = msgId ?? 'fixture-${_chatSeq++}';
    final stamp = ts ?? DateTime.now().toUtc().toIso8601String();
    _chat.putIfAbsent(chatGroupId, () => []).add(ChatMessage(
          chatGroupId: chatGroupId,
          msgId: id,
          ts: stamp,
          role: role,
          content: content,
          embedded: embed,
          author: author,
          metadata: metadata,
        ));
    return AppendedMessage(msgId: id, ts: stamp);
  }

  @override
  Future<ChatPage> listMessages(
    String chatGroupId, {
    String? since,
    String? until,
    int limit = 100,
    String? cursor,
    String direction = 'desc',
  }) async {
    final all = [...?_chat[chatGroupId]]..sort((a, b) => a.ts.compareTo(b.ts));
    final ordered = direction == 'asc' ? all : all.reversed.toList();
    return ChatPage(messages: ordered.take(limit).toList());
  }

  // ── admin ops (synthetic offline values) ────────────────────

  @override
  Future<QuotaSnapshot> adminQuota() async => const QuotaSnapshot(
        queriesRemaining: 60,
        writesRemaining: 30,
        embedsRemaining: 60,
        concurrentSessionsInUse: 0,
      );

  @override
  Future<AuditDrift> adminAudit() async =>
      const AuditDrift(markdownNotInDuckdb: [], indexedButNoMarkdown: []);

  @override
  Future<int> adminDeleteChatHistory({String? chatGroupId, String? beforeTs}) async {
    if (chatGroupId == null) {
      final n = _chat.values.fold<int>(0, (a, l) => a + l.length);
      _chat.clear();
      return n;
    }
    final removed = _chat.remove(chatGroupId)?.length ?? 0;
    return removed;
  }

  @override
  Future<List<LaneSummary>> adminListLanes() async => throw notYetImplemented('admin_list_lanes');

  @override
  Future<List<LaneKey>> adminLaneKeys(String lane, {String? prefix, int limit = 100}) async =>
      throw notYetImplemented('admin_lane_keys');

  @override
  Future<LaneBlob> adminLaneBlob(String lane, String key) async =>
      throw notYetImplemented('admin_lane_blob');

  @override
  Future<QueryResult> adminIndexQuery(String table, {Map<String, Object?>? filter, int? limit}) async =>
      throw notYetImplemented('admin_index_query');

  // ── health ──────────────────────────────────────────────────

  @override
  Future<HealthInfo> healthz() async =>
      HealthInfo(ok: true, checkedAt: DateTime.now().toUtc());

  @override
  Future<VersionInfo> version() async => VersionInfo(
        app: 'fixture-client',
        version: '0.1.0',
        gitSha: 'fixture',
        capabilities: {
          BackendCapability.agentReadTools,
          if (_writeEnabled) BackendCapability.agentWriteTools,
        },
      );

  @override
  void close() {}

  // ── helpers ─────────────────────────────────────────────────

  String? _resolveRef(WikilinkRef ref) {
    if (ref.id == null) return null;
    final qualified = ref.skill != null ? '${ref.skill}__${ref.id}' : ref.id!;
    if (_pages.containsKey(qualified)) return qualified;
    if (_pages.containsKey(ref.id!)) return ref.id!;
    return null;
  }

  bool _matches(dynamic actual, Object? expected) {
    if (expected == null) return actual == null;
    return actual?.toString() == expected.toString();
  }

  List<String> _stringList(dynamic v) {
    if (v is List) return v.map((e) => e.toString()).toList();
    return const <String>[];
  }
}
