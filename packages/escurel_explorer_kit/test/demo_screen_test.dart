// Behavioral coverage for the capability demo's UI → client wiring.
//
// Runs under `flutter test` (VM, headless, deterministic) — no
// browser, no network. A recording client captures every call the
// panels make, so each test proves a button/tap reaches the right
// EscurelClient method with the right arguments. This is the reliable
// gate-level behavioral check; scripts/drive-demo.sh (flutter drive)
// and scripts/verify-demo.sh (rodney) add real-browser coverage on
// top.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/demo/demo_screen.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late _RecordingClient rec;

  // The TextField inside a _Labelled(label: …) wrapper.
  Finder editable(String label) => find.descendant(
        of: find.bySemanticsLabel(label),
        matching: find.byType(TextField),
      );

  Future<void> pumpDemo(WidgetTester tester) async {
    rec = _RecordingClient();
    await tester.pumpWidget(
      ProviderScope(
        overrides: [escurelClientProvider.overrideWithValue(rec)],
        child: const MaterialApp(home: DemoScreen()),
      ),
    );
    await tester.pumpAndSettle();
  }

  testWidgets('Search submit calls search() with the typed query', (tester) async {
    await pumpDemo(tester);
    await tester.enterText(editable('search-input'), 'acme churn');
    // 'Search' text also labels the tab; target the submit button.
    await tester.tap(find.widgetWithText(ElevatedButton, 'Search'));
    await tester.pump();
    expect(rec.searchedQueries, contains('acme churn'));
  });

  testWidgets('Author Validate + Save call validate()/updatePage()', (tester) async {
    await pumpDemo(tester);
    await tester.tap(find.text('Author'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Validate'));
    await tester.pump();
    expect(rec.validateCalls, 1);

    await tester.tap(find.text('Save (update_page)'));
    await tester.pump();
    expect(rec.updatedPageIds, hasLength(1));
    expect(rec.updatedPageIds.single, contains('note/demo'));
  });

  testWidgets('Chat Append calls appendMessage() with group + content', (tester) async {
    await pumpDemo(tester);
    await tester.tap(find.text('Chat'));
    await tester.pumpAndSettle();

    await tester.enterText(editable('chat-group'), 'room-x');
    await tester.enterText(editable('chat-content'), 'a message');
    await tester.tap(find.text('Append'));
    await tester.pump();

    expect(rec.appended, hasLength(1));
    expect(rec.appended.single.$1, 'room-x');
    expect(rec.appended.single.$2, 'a message');
  });

  testWidgets('Ops Load calls adminQuota() and adminAudit()', (tester) async {
    await pumpDemo(tester);
    await tester.tap(find.text('Ops'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Load ops (admin)'));
    await tester.pumpAndSettle();

    expect(rec.quotaCalls, 1);
    expect(rec.auditCalls, 1);
    // The quota line rendered from the returned snapshot.
    expect(find.textContaining('quota —'), findsOneWidget);
  });
}

/// Records the calls the demo panels make; returns canned values so
/// the widgets render a result. Only the methods the demo uses are
/// meaningfully implemented; the rest throw (never reached).
class _RecordingClient implements EscurelClient {
  final List<String> searchedQueries = [];
  int validateCalls = 0;
  final List<String> updatedPageIds = [];
  final List<(String, String)> appended = []; // (group, content)
  int quotaCalls = 0;
  int auditCalls = 0;

  @override
  Future<SearchResult> search({
    required String q,
    int k = 10,
    SearchGranularity granularity = SearchGranularity.block,
    PageTypeFilter pageType = PageTypeFilter.any,
    String? skill,
    String? asOf,
  }) async {
    searchedQueries.add(q);
    return const SearchResult(hits: [], granularity: SearchGranularity.block);
  }

  @override
  Future<ValidationResult> validate(String content, {String? asPageId}) async {
    validateCalls++;
    return const ValidationResult(issues: []);
  }

  @override
  Future<UpdateResult> updatePage(String pageId, String content, {String? baseVersion}) async {
    updatedPageIds.add(pageId);
    return const UpdateResult(ok: true, issues: [], newVersion: 'v1');
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
  }) async {
    appended.add((chatGroupId, content));
    return const AppendedMessage(msgId: 'm1', ts: '2026-05-26T00:00:00Z');
  }

  @override
  Future<ChatPage> listMessages(
    String chatGroupId, {
    String? since,
    String? until,
    int limit = 100,
    String? cursor,
    String direction = 'desc',
  }) async =>
      const ChatPage(messages: []);

  @override
  Future<QuotaSnapshot> adminQuota() async {
    quotaCalls++;
    return const QuotaSnapshot(
      queriesRemaining: 60,
      writesRemaining: 30,
      embedsRemaining: 60,
      concurrentSessionsInUse: 0,
    );
  }

  @override
  Future<AuditDrift> adminAudit() async {
    auditCalls++;
    return const AuditDrift(markdownNotInDuckdb: [], indexedButNoMarkdown: []);
  }

  // Unused by the demo panels — never invoked in these tests.
  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnimplementedError('${invocation.memberName} not used by the demo');
}
