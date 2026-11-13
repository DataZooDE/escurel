// Widget test for the per-page cost badge: an admin sees what the page's
// runs cost over the last 30 days (`usage_report{group_by:[page]}`); a
// non-admin — for whom usage_report is refused — sees no badge at all,
// and neither does a page no run targeted.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/page_cost_badge.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _page = 'markdown/instances/engagement/hoffmann-spine.md';

/// Answers `whoami` and `usage_report` only; any other surface routes to
/// `noSuchMethod` and fails loudly. Records the report request so the
/// 30-day window is asserted, not assumed.
class _UsageClient implements EscurelClient {
  _UsageClient({required this.isAdmin});

  final bool isAdmin;
  List<String>? groupBy;
  String? since;

  @override
  Future<Whoami> whoami() async =>
      Whoami(subject: 'me', tenant: 't', isAdmin: isAdmin);

  @override
  Future<UsageReport> usageReport({
    required List<String> groupBy,
    String? since,
    String? until,
    String? skill,
  }) async {
    if (!isAdmin) throw StateError('usage_report is admin-only');
    this.groupBy = groupBy;
    this.since = since;
    return const UsageReport(
      rows: [
        UsageRow(
          group: {'page': _page},
          runs: 7,
          byStatus: {'processed': 7},
          costUsd: 0.4234,
        ),
      ],
    );
  }

  @override
  void close() {}

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<void> _pump(WidgetTester tester, EscurelClient client, String pageId) =>
    tester
        .pumpWidget(
          ProviderScope(
            overrides: [escurelClientProvider.overrideWithValue(client)],
            child: MaterialApp(
              home: Scaffold(body: PageCostBadge(pageId: pageId)),
            ),
          ),
        )
        .then((_) => tester.pumpAndSettle());

void main() {
  testWidgets('an admin sees the page cost over 30 days', (tester) async {
    final client = _UsageClient(isAdmin: true);
    await _pump(tester, client, _page);

    expect(find.bySemanticsLabel('page-cost-badge'), findsOneWidget);
    expect(find.text(r'$0.42 · 7 runs (30d)'), findsOneWidget);
    expect(client.groupBy, ['page']);
    final since = DateTime.parse(client.since!);
    final days = DateTime.now().toUtc().difference(since).inHours / 24;
    expect(days, closeTo(30, 0.1));
  });

  testWidgets('a non-admin sees no badge (and no error)', (tester) async {
    await _pump(tester, _UsageClient(isAdmin: false), _page);
    expect(find.bySemanticsLabel('page-cost-badge'), findsNothing);
    expect(find.textContaining('error'), findsNothing);
  });

  testWidgets('a page no run targeted shows no badge', (tester) async {
    await _pump(
      tester,
      _UsageClient(isAdmin: true),
      'markdown/instances/customer/acme.md',
    );
    expect(find.bySemanticsLabel('page-cost-badge'), findsNothing);
  });
}
