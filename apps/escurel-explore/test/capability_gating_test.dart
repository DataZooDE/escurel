/// Widget tests for the feature-flag gating wired in PR-6b.
///
/// Both scenarios use a real [FixtureEscurelClient] for the client
/// (its `version()` reports only `agentReadTools`) and a stub
/// version() override for the "all capabilities" case to verify the
/// inverse path.
library;

import 'package:escurel_explore/app.dart';
import 'package:escurel_explorer_kit/client/errors.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

EscurelClient _fixture() {
  return FixtureEscurelClient.fromSources(
    skillFiles: const {
      'customer.md': '''---
type: skill
id: customer
description: A buying organisation.
---

# customer
''',
    },
    instanceFiles: const {
      'customer__acme.md': '''---
type: instance
skill: customer
id: acme
name: Acme Ltd
---

# Acme
''',
    },
  );
}

/// A test-only client that lets each test specify exactly which
/// capabilities the backend reports. Everything else delegates to
/// the fixture.
class _CapStubClient implements EscurelClient {
  _CapStubClient({required this.inner, required this.caps});

  final EscurelClient inner;
  final Set<BackendCapability> caps;

  @override
  Future<VersionInfo> version() async => VersionInfo(
    app: 'stub',
    version: '0.0.0',
    gitSha: 'stub',
    capabilities: caps,
  );

  // Delegate everything else.
  @override
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
    String? asOf,
  }) => inner.search(
    q: q,
    k: k,
    granularity: granularity,
    pageType: pageType,
    skill: skill,
    asOf: asOf,
  );

  @override
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) =>
      inner.resolve(wikilink, scenario: scenario);

  @override
  Future<ExpandResult> expand(
    String pageId, {
    String? anchor,
    String? version,
    String? asOf,
    String? scenario,
  }) => inner.expand(
    pageId,
    anchor: anchor,
    version: version,
    asOf: asOf,
    scenario: scenario,
  );

  @override
  Future<List<Neighbour>> neighbours(
    String pageId, {
    LinkDirection direction = LinkDirection.both,
    String? linkSkill,
    String? asOf,
    String? scenario,
  }) => inner.neighbours(
    pageId,
    direction: direction,
    linkSkill: linkSkill,
    asOf: asOf,
    scenario: scenario,
  );

  @override
  Future<List<SkillSummary>> listSkills() => inner.listSkills();

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
    String? scenario,
  }) => inner.listInstances(
    skillId,
    filter: filter,
    orderBy: orderBy,
    limit: limit,
    asOf: asOf,
    scenario: scenario,
  );

  @override
  Future<QueryResult> runStoredQuery(
    String queryId, {
    Map<String, Object?> params = const {},
  }) => inner.runStoredQuery(queryId, params: params);

  @override
  Future<List<Event>> listInbox({int? limit}) => inner.listInbox(limit: limit);
  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) =>
      inner.listEvents(instancePageId, limit: limit);
  @override
  Future<List<String>> listSnapshots(String pageId) =>
      inner.listSnapshots(pageId);
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
  }) => inner.captureEvent(
    at: at,
    source: source,
    mime: mime,
    labelSkill: labelSkill,
    instancePageId: instancePageId,
    title: title,
    body: body,
    provenance: provenance,
  );

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) =>
      inner.validate(content, asPageId: asPageId);

  @override
  Future<UpdateResult> updatePage(
    String pageId,
    String content, {
    String? baseVersion,
  }) => inner.updatePage(pageId, content, baseVersion: baseVersion);

  @override
  Future<Session> openSession(String pageId) => inner.openSession(pageId);

  @override
  Future<ApplyOpResult> applyOp(String session, CrdtOp op) =>
      inner.applyOp(session, op);

  @override
  Future<CloseResult> closeSession(String session, {bool commit = true}) =>
      inner.closeSession(session, commit: commit);

  @override
  Stream<AwarenessEvent> awareness(String pageId) => inner.awareness(pageId);

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
  }) => inner.appendMessage(
    chatGroupId: chatGroupId,
    role: role,
    content: content,
    author: author,
    ts: ts,
    metadata: metadata,
    msgId: msgId,
    embed: embed,
  );

  @override
  Future<ChatPage> listMessages(
    String chatGroupId, {
    String? since,
    String? until,
    int limit = 100,
    String? cursor,
    String direction = 'desc',
  }) => inner.listMessages(
    chatGroupId,
    since: since,
    until: until,
    limit: limit,
    cursor: cursor,
    direction: direction,
  );

  @override
  Future<QuotaSnapshot> adminQuota() => inner.adminQuota();

  @override
  Future<AuditDrift> adminAudit() => inner.adminAudit();

  @override
  Future<WebhookDeliveries> adminWebhookDeliveries({int limit = 100}) =>
      inner.adminWebhookDeliveries(limit: limit);

  @override
  Future<int> adminDeleteChatHistory({String? chatGroupId, String? beforeTs}) =>
      inner.adminDeleteChatHistory(
        chatGroupId: chatGroupId,
        beforeTs: beforeTs,
      );

  @override
  Future<void> addGroupMember(String groupId, String subject) =>
      inner.addGroupMember(groupId, subject);

  @override
  Future<void> removeGroupMember(String groupId, String subject) =>
      inner.removeGroupMember(groupId, subject);

  @override
  Future<List<GroupMember>> listGroupMembers(String groupId) =>
      inner.listGroupMembers(groupId);

  @override
  Future<List<LaneSummary>> adminListLanes() => inner.adminListLanes();

  @override
  Future<List<LaneKey>> adminLaneKeys(
    String lane, {
    String? prefix,
    int limit = 100,
  }) => inner.adminLaneKeys(lane, prefix: prefix, limit: limit);

  @override
  Future<LaneBlob> adminLaneBlob(String lane, String key) =>
      inner.adminLaneBlob(lane, key);

  @override
  Future<QueryResult> adminIndexQuery(
    String table, {
    Map<String, Object?>? filter,
    int? limit,
    String? asOf,
  }) => inner.adminIndexQuery(table, filter: filter, limit: limit);

  @override
  Future<HealthInfo> healthz() => inner.healthz();

  @override
  Future<void> registerCredential({
    required String name,
    required String connector,
    required String secret,
  }) => inner.registerCredential(
    name: name,
    connector: connector,
    secret: secret,
  );

  @override
  Future<List<CredentialInfo>> listCredentials() => inner.listCredentials();

  @override
  Future<void> deleteCredential(String name) => inner.deleteCredential(name);

  @override
  Future<List<BindingStatus>> validateBindings() => inner.validateBindings();

  @override
  Future<String> createSqlInstance({
    required String skill,
    required String id,
    String? overlayBody,
  }) => inner.createSqlInstance(skill: skill, id: id, overlayBody: overlayBody);

  @override
  Future<IngestOutcome> ingestUpload({
    required String contentType,
    required List<int> bytes,
    String? title,
  }) => inner.ingestUpload(contentType: contentType, bytes: bytes, title: title);

  @override
  void close() => inner.close();
}

Widget _appWith(EscurelClient client) {
  return ProviderScope(
    overrides: [escurelClientProvider.overrideWithValue(client)],
    child: const EscurelExploreApp(),
  );
}

void main() {
  testWidgets('topbar shows read-only chip when write capability is absent', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _fixture();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client));
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('topbar.read_only_chip')), findsOneWidget);
    expect(find.text('read-only'), findsOneWidget);
  });

  testWidgets('topbar hides read-only chip when write capability is present', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _CapStubClient(
      inner: _fixture(),
      caps: const {
        BackendCapability.agentReadTools,
        BackendCapability.agentWriteTools,
      },
    );
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client));
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('topbar.read_only_chip')), findsNothing);
  });

  testWidgets('status bar reflects backend version + capability count', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _fixture();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client));
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('status_bar.backend')), findsOneWidget);
    // Fixture client's version() reports app="fixture-client",
    // version="0.1.0", capabilities={agentReadTools} → 1 capability.
    final label =
        (tester.widget(find.byKey(const ValueKey('status_bar.backend')))
                as Text)
            .data!;
    expect(label, contains('fixture-client'));
    expect(label, contains('0.1.0'));
    expect(label, contains('1 capabilities'));
  });
}

// Ensure unused import warning silenced — the error import is here
// for future stub variants that throw client errors.
// ignore: unused_element
void _silenceUnused() => const EscurelTransportException('').toString();
