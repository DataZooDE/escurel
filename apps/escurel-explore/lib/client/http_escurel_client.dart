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

  // ── write tools ─────────────────────────────────────────────

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async {
    final result = await _call('validate', {
      'content': content,
      'as_page_id': ?asPageId,
    });
    return ValidationResult(issues: _issuesFromJson(result['issues']));
  }

  @override
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion}) async {
    final result = await _call('update_page', {
      'page_id': pageId,
      'content': content,
      'base_version': ?baseVersion,
    });
    return UpdateResult(
      ok: (result['ok'] as bool?) ?? true,
      issues: _issuesFromJson(result['issues']),
      newVersion: result['new_version'] as String?,
    );
  }

  // ── chat history (M-Chat) ───────────────────────────────────

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
    final result = await _call('append_message', {
      'chat_group_id': chatGroupId,
      'role': role,
      'content': content,
      'author': ?author,
      'ts': ?ts,
      'metadata': ?metadata,
      'msg_id': ?msgId,
      'embed': embed,
    });
    return AppendedMessage(
      msgId: (result['msg_id'] as String?) ?? '',
      ts: (result['ts'] as String?) ?? '',
    );
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
    final result = await _call('list_messages', {
      'chat_group_id': chatGroupId,
      'since': ?since,
      'until': ?until,
      'limit': limit,
      'cursor': ?cursor,
      'direction': direction,
    });
    final messages = (result['messages'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map(_chatMessageFromJson)
        .toList();
    return ChatPage(messages: messages, nextCursor: result['next_cursor'] as String?);
  }

  ChatMessage _chatMessageFromJson(Map<String, dynamic> j) => ChatMessage(
        chatGroupId: (j['chat_group_id'] as String?) ?? '',
        msgId: (j['msg_id'] as String?) ?? '',
        ts: (j['ts'] as String?) ?? '',
        role: (j['role'] as String?) ?? '',
        content: (j['content'] as String?) ?? '',
        embedded: (j['embedded'] as bool?) ?? false,
        author: j['author'] as String?,
        metadata: (j['metadata'] as Map?)?.cast<String, Object?>(),
      );

  // ── live mode — handled by LiveSession over /ws (see editor) ──

  @override
  Future<Session> openSession(String pageId) async {
    final result = await _call('open_session', {'page_id': pageId});
    return Session(
      id: (result['session'] as String?) ?? '',
      pageId: pageId,
      headVersion: (result['head_version'] as String?) ?? '',
      content: (result['content'] as String?) ?? '',
    );
  }

  @override
  Future<ApplyOpResult> applyOp(String session, CrdtOp op) async {
    // The HTTP `apply_op` tool expects a base64 Loro op blob in
    // `op`. The browser live-edit path drives ops over `/ws`
    // instead (see the Live panel); this HTTP method is kept for
    // completeness / non-CRDT callers.
    final result = await _call('apply_op', {
      'session': session,
      'op': op.payload['base64'] ?? '',
    });
    return ApplyOpResult(ok: (result['ok'] as bool?) ?? true);
  }

  @override
  Future<CloseResult> closeSession(String session, {bool commit = true}) async {
    final result = await _call('close_session', {'session': session, 'commit': commit});
    return CloseResult(
      finalVersion: (result['merged_version'] as String?) ??
          (result['final_version'] as String?) ??
          '',
      issues: _issuesFromJson(result['issues']),
    );
  }

  @override
  Stream<AwarenessEvent> awareness(String pageId) async* {
    throw notYetImplemented('awareness (ws)');
  }

  // ── admin ops tools (escurel-admin role) ────────────────────

  @override
  Future<QuotaSnapshot> adminQuota() async {
    final r = await _call('admin_quota', const {});
    return QuotaSnapshot(
      queriesRemaining: (r['queries_remaining'] as num?)?.toInt() ?? 0,
      writesRemaining: (r['writes_remaining'] as num?)?.toInt() ?? 0,
      embedsRemaining: (r['embeds_remaining'] as num?)?.toInt() ?? 0,
      concurrentSessionsInUse: (r['concurrent_sessions_in_use'] as num?)?.toInt() ?? 0,
    );
  }

  @override
  Future<AuditDrift> adminAudit() async {
    final r = await _call('admin_audit', const {});
    return AuditDrift(
      markdownNotInDuckdb:
          (r['markdown_not_in_duckdb'] as List? ?? const []).map((e) => e.toString()).toList(),
      indexedButNoMarkdown:
          (r['indexed_but_no_markdown'] as List? ?? const []).map((e) => e.toString()).toList(),
    );
  }

  @override
  Future<int> adminDeleteChatHistory({String? chatGroupId, String? beforeTs}) async {
    final r = await _call('admin_delete_chat_history', {
      'chat_group_id': ?chatGroupId,
      'before_ts': ?beforeTs,
    });
    return (r['deleted'] as num?)?.toInt() ?? 0;
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
  Future<QueryResult> adminIndexQuery(String table, {Map<String, Object?>? filter, int? limit}) async {
    final result = await _call('admin_index_query', {
      'table': table,
      'limit': ?limit,
    });
    final columns = (result['schema'] as List? ?? const [])
        .cast<Map<String, dynamic>>()
        .map((c) => QueryColumn(name: (c['name'] as String?) ?? '', dartType: (c['type'] as String?) ?? 'dynamic'))
        .toList();
    final rows = (result['rows'] as List? ?? const []).cast<Map<String, dynamic>>().toList();
    return QueryResult(columns: columns, rows: rows);
  }

  List<Issue> _issuesFromJson(Object? raw) =>
      (raw as List? ?? const []).cast<Map<String, dynamic>>().map((j) {
        final sev = switch (j['severity'] as String?) {
          'error' => IssueSeverity.error,
          'warning' => IssueSeverity.warning,
          _ => IssueSeverity.info,
        };
        return Issue(
          severity: sev,
          code: (j['code'] as String?) ?? '',
          // The server carries `location` (an anchor/path string); fold
          // it into the message so the UI shows where without a schema
          // change to the line/column ints.
          message: [
            (j['message'] as String?) ?? '',
            if ((j['location'] as String?)?.isNotEmpty ?? false) '(${j['location']})',
            if ((j['suggestion'] as String?)?.isNotEmpty ?? false) '— ${j['suggestion']}',
          ].where((s) => s.isNotEmpty).join(' '),
        );
      }).toList();

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
