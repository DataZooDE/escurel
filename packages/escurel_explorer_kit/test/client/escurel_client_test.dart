import 'package:escurel_explorer_kit/client/errors.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
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

    test(
      'notYetImplemented returns EscurelUnsupportedException with tool name',
      () {
        final ex = notYetImplemented('search');
        expect(ex, isA<EscurelUnsupportedException>());
        expect(ex.message, contains('search'));
      },
    );
  });

  group('EscurelClient interface', () {
    test(
      'a partial implementation compiles and surfaces unsupported tools',
      () async {
        final EscurelClient client = _StubClient();

        // Use a tool with no implementation to assert the surface plumbs through.
        await expectLater(
          client.search(q: 'hello'),
          throwsA(isA<EscurelUnsupportedException>()),
        );
      },
    );
  });

  group('DTO defaults', () {
    test('ValidationResult.isOk is true when there are no errors', () {
      const ok = ValidationResult(
        issues: [
          Issue(severity: IssueSeverity.warning, code: 'W1', message: 'minor'),
        ],
      );
      expect(ok.isOk, isTrue);
    });

    test('ValidationResult.isOk is false when any error is present', () {
      const broken = ValidationResult(
        issues: [
          Issue(severity: IssueSeverity.error, code: 'E1', message: 'bad'),
        ],
      );
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
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) async =>
      throw notYetImplemented('resolve');

  @override
  Future<ExpandResult> expand(
    String pageId, {
    String? anchor,
    String? version,
    String? asOf,
    String? scenario,
  }) async => throw notYetImplemented('expand');

  @override
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
    String? asOf,
    String? scenario,
  }) async => throw notYetImplemented('neighbours');

  @override
  Future<List<SkillSummary>> listSkills() async =>
      throw notYetImplemented('list_skills');

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
  Future<List<Event>> listInbox({int? limit}) async =>
      throw notYetImplemented('list_inbox');
  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async =>
      throw notYetImplemented('list_events');
  @override
  Future<List<String>> listSnapshots(String pageId) async =>
      throw notYetImplemented('list_snapshots');
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
  }) async => throw notYetImplemented('capture_event');

  @override
  Future<QueryResult> runStoredQuery(
    String queryId, {
    Map<String, Object?> params = const {},
  }) async => throw notYetImplemented('run_stored_query');

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async =>
      throw notYetImplemented('validate');

  @override
  Future<UpdateResult> updatePage(
    String pageId,
    String content, {
    String? baseVersion,
  }) async => throw notYetImplemented('update_page');

  @override
  Future<Session> openSession(String pageId) async =>
      throw notYetImplemented('open_session');

  @override
  Future<ApplyOpResult> applyOp(String session, CrdtOp op) async =>
      throw notYetImplemented('apply_op');

  @override
  Future<CloseResult> closeSession(
    String session, {
    bool commit = true,
  }) async => throw notYetImplemented('close_session');

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
  Future<QuotaSnapshot> adminQuota() async =>
      throw notYetImplemented('admin_quota');

  @override
  Future<AuditDrift> adminAudit() async =>
      throw notYetImplemented('admin_audit');

  @override
  Future<WebhookDeliveries> adminWebhookDeliveries({int limit = 100}) async =>
      throw notYetImplemented('admin_webhook_deliveries');

  @override
  Future<int> adminDeleteChatHistory({
    String? chatGroupId,
    String? beforeTs,
  }) async => throw notYetImplemented('admin_delete_chat_history');

  @override
  Future<void> addGroupMember(String groupId, String subject) async =>
      throw notYetImplemented('add_group_member');

  @override
  Future<void> removeGroupMember(String groupId, String subject) async =>
      throw notYetImplemented('remove_group_member');

  @override
  Future<List<GroupMember>> listGroupMembers(String groupId) async =>
      throw notYetImplemented('list_group_members');

  @override
  Future<List<LaneSummary>> adminListLanes() async =>
      throw notYetImplemented('admin_list_lanes');

  @override
  Future<List<LaneKey>> adminLaneKeys(
    String lane, {
    String? prefix,
    int limit = 100,
  }) async => throw notYetImplemented('admin_lane_keys');

  @override
  Future<LaneBlob> adminLaneBlob(String lane, String key) async =>
      throw notYetImplemented('admin_lane_blob');

  @override
  Future<QueryResult> adminIndexQuery(
    String table, {
    Map<String, Object?>? filter,
    int? limit,
    String? asOf,
  }) async => throw notYetImplemented('admin_index_query');

  @override
  Future<HealthInfo> healthz() async => throw notYetImplemented('healthz');

  @override
  Future<VersionInfo> version() async => throw notYetImplemented('version');

  @override
  Future<void> registerCredential({
    required String name,
    required String connector,
    required String secret,
  }) async => throw notYetImplemented('register_credential');

  @override
  Future<List<CredentialInfo>> listCredentials() async =>
      throw notYetImplemented('list_credentials');

  @override
  Future<List<PackSubscriptionInfo>> listPacks() async =>
      throw notYetImplemented('list_packs');

  @override
  Future<PackOpResult> importPack(
    String manifestJson,
    String tarballBase64, {
    bool allowVerticalMismatch = false,
  }) async => throw notYetImplemented('import_pack');

  @override
  Future<PackOpResult> rebasePack(
    String manifestJson,
    String tarballBase64, {
    bool acknowledgeConflicts = false,
    bool dryRun = false,
  }) async => throw notYetImplemented('rebase_pack');

  @override
  Future<PackOpResult> unsubscribePack(String packId) async =>
      throw notYetImplemented('unsubscribe_pack');

  @override
  Future<void> deleteCredential(String name) async =>
      throw notYetImplemented('delete_credential');

  @override
  Future<List<BindingStatus>> validateBindings() async =>
      throw notYetImplemented('validate_bindings');

  @override
  Future<void> registerEndpoint({
    required String name,
    required String kind,
    required String baseUrl,
    String auth = 'none',
    String? authHeader,
    String? secret,
  }) async => throw notYetImplemented('register_endpoint');

  @override
  Future<List<EndpointInfo>> listEndpoints() async =>
      throw notYetImplemented('list_endpoints');

  @override
  Future<void> deleteEndpoint(String name) async =>
      throw notYetImplemented('delete_endpoint');

  @override
  Future<List<EndpointHealth>> validateEndpoints() async =>
      throw notYetImplemented('validate_endpoints');

  @override
  Future<String> createSqlInstance({
    required String skill,
    required String id,
    String? overlayBody,
  }) async => throw notYetImplemented('create_sql_instance');

  @override
  Future<QueryResult> queryInstance(
    String queryRef, {
    Map<String, Object?> params = const {},
  }) async => throw notYetImplemented('query_instance');

  @override
  Future<String> createRemoteInstance({
    required String skill,
    required String id,
    String? overlayBody,
  }) async => throw notYetImplemented('create_remote_instance');

  @override
  Future<IngestOutcome> ingestUpload({
    required String contentType,
    required List<int> bytes,
    String? title,
  }) async => throw notYetImplemented('ingest/upload');

  @override
  void close() {}
}
