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
  });

  final String id;
  final String skill;
  final md.PageType pageType;
  final Map<String, dynamic> frontmatter;
  final String body;
  final List<WikilinkRef> wikilinksOut;
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

    return FixtureEscurelClient._(pages);
  }

  FixtureEscurelClient._(this._pages);

  final Map<String, _ParsedPage> _pages;

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
    return _pages.values
        .where((p) => p.pageType == md.PageType.skill)
        .map((p) => SkillSummary(
              id: p.id,
              description: (p.frontmatter['description'] as String?) ?? '',
              requiredFrontmatter: _stringList(p.frontmatter['required_frontmatter']),
              optionalFrontmatter: _stringList(p.frontmatter['optional_frontmatter']),
            ))
        .toList();
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

    if (direction == LinkDirection.outgoing || direction == LinkDirection.both) {
      final p = _pages[pageId];
      if (p != null) {
        for (final ref in p.wikilinksOut) {
          final dst = _resolveRef(ref);
          if (dst == null) continue;
          if (linkSkill != null && ref.skill != linkSkill) continue;
          out.add(Neighbour(src: p.id, dst: dst, linkSkill: ref.skill ?? ''));
        }
      }
    }

    if (direction == LinkDirection.incoming || direction == LinkDirection.both) {
      for (final cand in _pages.values) {
        for (final ref in cand.wikilinksOut) {
          final dst = _resolveRef(ref);
          if (dst != pageId) continue;
          if (linkSkill != null && ref.skill != linkSkill) continue;
          out.add(Neighbour(src: cand.id, dst: pageId, linkSkill: ref.skill ?? ''));
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

  // ── unimplemented surfaces (need a real server) ─────────────

  // Events/inbox are an HTTP-backend surface; fixture mode has none.
  @override
  Future<List<Event>> listInbox({int? limit}) async => const [];

  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async => const [];

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
  }) async =>
      throw notYetImplemented('capture_event');

  @override
  Future<QueryResult> runStoredQuery(String queryId, {Map<String, Object?> params = const {}}) async =>
      throw notYetImplemented('run_stored_query');

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async =>
      throw notYetImplemented('validate');

  @override
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion}) async =>
      throw notYetImplemented('update_page');

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
  Future<VersionInfo> version() async => const VersionInfo(
        app: 'fixture-client',
        version: '0.1.0',
        gitSha: 'fixture',
        capabilities: {BackendCapability.agentReadTools},
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
