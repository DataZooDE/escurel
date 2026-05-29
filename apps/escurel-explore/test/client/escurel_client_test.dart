import 'package:escurel_explore/client/errors.dart';
import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('errors', () {
    test('sealed hierarchy — switch is exhaustive', () {
      EscurelClientException e = const EscurelTransportException('boom');
      final label = switch (e) {
        EscurelTransportException _ => 'transport',
        EscurelToolException _ => 'tool',
        EscurelUnsupportedException _ => 'unsupported',
      };
      expect(label, 'transport');
    });

    test('notYetImplemented returns EscurelUnsupportedException with tool name', () {
      final ex = notYetImplemented('search');
      expect(ex, isA<EscurelUnsupportedException>());
      expect(ex.message, contains('search'));
    });
  });

  group('EscurelClient interface', () {
    test('a partial implementation compiles and surfaces unsupported tools', () async {
      final EscurelClient client = _StubClient();

      // Use a tool with no implementation to assert the surface plumbs through.
      await expectLater(
        client.search(q: 'hello'),
        throwsA(isA<EscurelUnsupportedException>()),
      );
    });
  });

  group('DTO defaults', () {
    test('ValidationResult.isOk is true when there are no errors', () {
      const ok = ValidationResult(issues: [
        Issue(severity: IssueSeverity.warning, code: 'W1', message: 'minor'),
      ]);
      expect(ok.isOk, isTrue);
    });

    test('ValidationResult.isOk is false when any error is present', () {
      const broken = ValidationResult(issues: [
        Issue(severity: IssueSeverity.error, code: 'E1', message: 'bad'),
      ]);
      expect(broken.isOk, isFalse);
    });
  });
}

/// Test-only stub implementation. Every method throws
/// [EscurelUnsupportedException]; exists purely to prove the
/// interface can be implemented partially without compile errors.
class _StubClient implements EscurelClient {
  @override
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
    String? asOf,
    String? scenario,
  }) async => throw notYetImplemented('search');

  @override
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) async => throw notYetImplemented('resolve');

  @override
  Future<ExpandResult> expand(String pageId, {String? anchor, String? version, String? asOf, String? scenario}) async =>
      throw notYetImplemented('expand');

  @override
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
    String? asOf,
    String? scenario,
  }) async => throw notYetImplemented('neighbours');

  @override
  Future<List<SkillSummary>> listSkills() async => throw notYetImplemented('list_skills');

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
    String? scenario,
  }) async => throw notYetImplemented('list_instances');

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
  }) async => throw notYetImplemented('append_message');

  @override
  Future<ChatPage> listMessages(
    String chatGroupId, {
    String? since,
    String? until,
    int limit = 100,
    String? cursor,
    String direction = 'desc',
  }) async => throw notYetImplemented('list_messages');

  @override
  Future<QuotaSnapshot> adminQuota() async => throw notYetImplemented('admin_quota');

  @override
  Future<AuditDrift> adminAudit() async => throw notYetImplemented('admin_audit');

  @override
  Future<int> adminDeleteChatHistory({String? chatGroupId, String? beforeTs}) async =>
      throw notYetImplemented('admin_delete_chat_history');

  @override
  Future<List<LaneSummary>> adminListLanes() async => throw notYetImplemented('admin_list_lanes');

  @override
  Future<List<LaneKey>> adminLaneKeys(String lane, {String? prefix, int limit = 100}) async =>
      throw notYetImplemented('admin_lane_keys');

  @override
  Future<LaneBlob> adminLaneBlob(String lane, String key) async =>
      throw notYetImplemented('admin_lane_blob');

  @override
  Future<QueryResult> adminIndexQuery(String table, {Map<String, Object?>? filter, int? limit, String? asOf}) async =>
      throw notYetImplemented('admin_index_query');

  @override
  Future<HealthInfo> healthz() async => throw notYetImplemented('healthz');

  @override
  Future<VersionInfo> version() async => throw notYetImplemented('version');

  @override
  void close() {}
}
