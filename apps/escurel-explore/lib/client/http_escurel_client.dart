/// MCP-over-HTTP implementation of [EscurelClient].
///
/// Wire format per `docs/spec/protocol.md` — every tool call is a
/// JSON-RPC 2.0 request with `method = "tools/call"` and
/// `params = { name: <tool>, arguments: {...} }`. The response is
/// a JSON-RPC result envelope wrapping the tool's documented output.
///
/// Auth: optional `Authorization: Bearer ${token}` header. v0 uses
/// `ESCUREL_EXPLORE_AUTH = none | bearer | oidc` from
/// [Env.auth]; oidc PKCE flow is a later PR (see
/// `docs/notes/discovered/<date>-explorer-auth-deferred.md` when
/// it lands).
///
/// Today this client implements every read tool, plus the substrate
/// `/healthz` and `/version` probes. Write tools, live mode, and
/// admin tools throw [EscurelUnsupportedException]; they arrive in a
/// follow-up once escurel-server's M3 work pins the write/live
/// envelope (the read envelope is already spec'd).
library;

import 'dart:async';

import 'package:dio/dio.dart';

import '../md/frontmatter.dart' as md;
import 'errors.dart';
import 'escurel_client.dart';
import 'models.dart';

class HttpEscurelClient implements EscurelClient {
  HttpEscurelClient({
    required String baseUrl,
    String? bearerToken,
    Dio? dio,
  }) : _dio = (dio ?? Dio())
          ..options.baseUrl = baseUrl
          ..options.connectTimeout = const Duration(seconds: 5)
          ..options.receiveTimeout = const Duration(seconds: 15)
          ..options.headers.addAll({
            'Accept': 'application/json',
            'Content-Type': 'application/json',
            'Authorization': ?(bearerToken != null ? 'Bearer $bearerToken' : null),
          });

  final Dio _dio;
  int _jsonRpcId = 0;

  // ── tool dispatch (MCP-over-HTTP envelope) ──────────────────

  Future<Map<String, dynamic>> _call(String tool, Map<String, dynamic> args) async {
    final id = ++_jsonRpcId;
    final body = {
      'jsonrpc': '2.0',
      'id': id,
      'method': 'tools/call',
      'params': {'name': tool, 'arguments': args},
    };

    Response<Map<String, dynamic>> resp;
    try {
      resp = await _dio.post<Map<String, dynamic>>('/mcp', data: body);
    } on DioException catch (e) {
      throw EscurelTransportException('POST /mcp failed: ${e.message}', cause: e);
    }

    final data = resp.data;
    if (data == null) {
      throw const EscurelTransportException('empty body from /mcp');
    }
    if (data['error'] is Map) {
      final err = data['error'] as Map;
      throw EscurelToolException(
        (err['message'] as String?) ?? 'tool error',
        code: (err['code']?.toString()) ?? 'unknown',
        details: err['data'] as Map<String, Object?>?,
      );
    }
    final result = data['result'];
    if (result is! Map<String, dynamic>) {
      throw EscurelTransportException('unexpected result shape: $result');
    }
    return result;
  }

  // ── read tools ──────────────────────────────────────────────

  @override
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
  }) async {
    final result = await _call('search', {
      'q': q,
      'k': k,
      'granularity': granularity.name,
      'page_type': pageType.name,
      'skill': ?skill,
    });
    final hits = (result['hits'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map(_hitFromJson)
        .toList();
    return SearchResult(hits: hits, granularity: granularity);
  }

  SearchHit _hitFromJson(Map<String, dynamic> j) => SearchHit(
        pageId: j['page_id'] as String,
        skill: j['skill'] as String? ?? '',
        score: (j['score'] as num?)?.toDouble() ?? 0.0,
        anchor: j['anchor'] as String?,
        snippet: j['snippet'] as String?,
      );

  @override
  Future<ResolveResult> resolve(String wikilink) async {
    final result = await _call('resolve', {'wikilink': wikilink});
    return ResolveResult(
      pageId: (result['page_id'] as String?) ?? '',
      skill: (result['skill'] as String?) ?? '',
      pageType: _pageTypeFromString(result['page_type'] as String?),
      exists: (result['exists'] as bool?) ?? false,
      description: result['description'] as String?,
      error: result['error'] as String?,
    );
  }

  @override
  Future<ExpandResult> expand(String pageId, {String? anchor, String? version}) async {
    final result = await _call('expand', {
      'page_id': pageId,
      'anchor': ?anchor,
      'version': ?version,
    });
    final blocks = (result['blocks'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((b) => Block(
              anchor: (b['anchor'] as String?) ?? '',
              content: (b['content'] as String?) ?? '',
            ))
        .toList();
    return ExpandResult(
      pageId: (result['page_id'] as String?) ?? pageId,
      skill: (result['skill'] as String?) ?? '',
      pageType: _pageTypeFromString(result['page_type'] as String?),
      frontmatter: Map<String, dynamic>.from(result['frontmatter'] as Map? ?? const {}),
      body: (result['body'] as String?) ?? '',
      blocks: blocks,
      wikilinksOut: (result['wikilinks_out'] as List? ?? const []).map((e) => e.toString()).toList(),
      version: result['version'] as String?,
    );
  }

  @override
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
  }) async {
    final result = await _call('neighbours', {
      'page_id': pageId,
      'direction': direction.name,
      'link_skill': ?linkSkill,
    });
    return (result['edges'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((e) => Neighbour(
              src: (e['src'] as String?) ?? '',
              dst: (e['dst'] as String?) ?? '',
              linkSkill: (e['link_skill'] as String?) ?? '',
              anchor: e['anchor'] as String?,
              linkVersion: e['link_version'] as String?,
            ))
        .toList();
  }

  @override
  Future<List<SkillSummary>> listSkills() async {
    final result = await _call('list_skills', const {});
    return (result['skills'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((s) => SkillSummary(
              id: (s['id'] as String?) ?? '',
              description: (s['description'] as String?) ?? '',
              requiredFrontmatter: (s['required_frontmatter'] as List? ?? const []).map((e) => e.toString()).toList(),
              optionalFrontmatter: (s['optional_frontmatter'] as List? ?? const []).map((e) => e.toString()).toList(),
            ))
        .toList();
  }

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
  }) async {
    final result = await _call('list_instances', {
      'skill_id': skillId,
      'filter': ?filter,
      'order_by': ?orderBy,
      'limit': ?limit,
    });
    return (result['instances'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((i) => InstanceSummary(
              id: (i['id'] as String?) ?? '',
              skill: (i['skill'] as String?) ?? skillId,
              frontmatter: Map<String, dynamic>.from(i['frontmatter'] as Map? ?? const {}),
            ))
        .toList();
  }

  @override
  Future<QueryResult> runStoredQuery(String queryId, {Map<String, Object?> params = const {}}) async {
    final result = await _call('run_stored_query', {
      'query_id': queryId,
      'params': params,
    });
    final columns = (result['columns'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((c) => QueryColumn(name: (c['name'] as String?) ?? '', dartType: (c['type'] as String?) ?? 'dynamic'))
        .toList();
    final rows = (result['rows'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .toList();
    return QueryResult(
      columns: columns,
      rows: rows,
      snapshotVersion: result['snapshot_version'] as String?,
    );
  }

  // ── write / live / admin — not yet implemented over HTTP ────

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async =>
      throw notYetImplemented('validate (http)');

  @override
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion}) async =>
      throw notYetImplemented('update_page (http)');

  @override
  Future<Session> openSession(String pageId) async => throw notYetImplemented('open_session (http)');

  @override
  Future<ApplyOpResult> applyOp(String session, CrdtOp op) async =>
      throw notYetImplemented('apply_op (http)');

  @override
  Future<CloseResult> closeSession(String session, {bool commit = true}) async =>
      throw notYetImplemented('close_session (http)');

  @override
  Stream<AwarenessEvent> awareness(String pageId) async* {
    throw notYetImplemented('awareness (ws)');
  }

  @override
  Future<List<LaneSummary>> adminListLanes() async => throw notYetImplemented('admin_list_lanes (http)');

  @override
  Future<List<LaneKey>> adminLaneKeys(String lane, {String? prefix, int limit = 100}) async =>
      throw notYetImplemented('admin_lane_keys (http)');

  @override
  Future<LaneBlob> adminLaneBlob(String lane, String key) async =>
      throw notYetImplemented('admin_lane_blob (http)');

  @override
  Future<QueryResult> adminIndexQuery(String table, {Map<String, Object?>? filter, int? limit}) async =>
      throw notYetImplemented('admin_index_query (http)');

  // ── substrate health (not MCP — plain HTTP) ─────────────────

  @override
  Future<HealthInfo> healthz() async {
    try {
      final r = await _dio.get<dynamic>('/healthz');
      return HealthInfo(ok: r.statusCode == 200, checkedAt: DateTime.now().toUtc());
    } on DioException {
      return HealthInfo(ok: false, checkedAt: DateTime.now().toUtc());
    }
  }

  @override
  Future<VersionInfo> version() async {
    final r = await _dio.get<Map<String, dynamic>>('/version');
    final data = r.data ?? const {};
    final capStrings = (data['capabilities'] as List? ?? const []).map((e) => e.toString()).toSet();
    return VersionInfo(
      app: (data['app'] as String?) ?? 'escurel-server',
      version: (data['version'] as String?) ?? 'unknown',
      gitSha: (data['git_sha'] as String?) ?? 'unknown',
      capabilities: _parseCapabilities(capStrings),
    );
  }

  Set<BackendCapability> _parseCapabilities(Set<String> raw) {
    final out = <BackendCapability>{BackendCapability.none};
    for (final s in raw) {
      final match = BackendCapability.values.where((c) => c.name == s);
      if (match.isNotEmpty) out.add(match.first);
    }
    return out;
  }

  @override
  void close() => _dio.close(force: true);

  static md.PageType _pageTypeFromString(String? s) =>
      s == 'skill' ? md.PageType.skill : md.PageType.instance;
}
