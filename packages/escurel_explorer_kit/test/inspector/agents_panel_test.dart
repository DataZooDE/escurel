// Widget test for the Dev Inspector's Agents panel: every `agent` profile
// instance with its harness, model and scopes, plus — for an admin — its
// last 30 days of runs and success rate from `usage_report{group_by:[agent]}`
// (keyed `agent:<id>`). Profiles come from the real fixture corpus; only
// whoami + usage_report are answered here, since the fixture has no runner.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/inspector/agents_panel.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _scribe = '''---
kind: instance
skill: agent
id: scribe
harness: claude
model: claude-sonnet-4
skills: ["[[skill::note]]", "[[skill::triage]]"]
scopes:
  tools: [search, expand, update_page, assign_event]
  write: ["markdown/instances/note/*"]
max_concurrent: 2
token_epoch: 1790071200
---
# scribe
''';

const _janitor = '''---
kind: instance
skill: agent
id: janitor
harness: echo
---
# janitor
''';

class _AgentsClient implements EscurelClient {
  _AgentsClient({required this.isAdmin, Map<String, String>? agents})
    : _inner = FixtureEscurelClient.fromSources(
        skillFiles: const {},
        instanceFiles:
            agents ?? const {'scribe.md': _scribe, 'janitor.md': _janitor},
      );

  final EscurelClient _inner;
  final bool isAdmin;
  List<String>? groupBy;

  @override
  Future<InstancePage> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
    String? scenario,
    String? cursor,
  }) => _inner.listInstances(skillId, limit: limit, cursor: cursor);

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
    return const UsageReport(
      rows: [
        UsageRow(
          group: {'agent': 'agent:scribe'},
          runs: 4,
          byStatus: {'processed': 3, 'failed': 1},
          costUsd: 0.42,
        ),
        // A run of a skill with no profile acts as `agent:<skill>`: not a
        // profile, so it must not surface as a row.
        UsageRow(group: {'agent': 'agent:note'}, runs: 9),
      ],
    );
  }

  @override
  void close() => _inner.close();

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1200, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: AgentsPanel())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('lists each agent profile with harness, model, scopes and runs', (
    tester,
  ) async {
    final client = _AgentsClient(isAdmin: true);
    await _pump(tester, client);

    expect(find.bySemanticsLabel('agents-panel'), findsOneWidget);
    expect(find.bySemanticsLabel('agent-row:scribe'), findsOneWidget);
    expect(find.bySemanticsLabel('agent-row:janitor'), findsOneWidget);
    expect(find.bySemanticsLabel('agent-row:note'), findsNothing);
    expect(client.groupBy, ['agent']);

    expect(find.text('claude · claude-sonnet-4'), findsOneWidget);
    expect(
      find.text(
        'tools: search, expand, update_page +1 · write: markdown/instances/note/*',
      ),
      findsOneWidget,
    );
    expect(find.text('4 runs · 75% ok · \$0.42'), findsOneWidget);

    // No model pinned, no scopes: the runner's choice, no extra limit.
    expect(find.text('echo · default model'), findsOneWidget);
    expect(find.text('tools: any · write: any'), findsOneWidget);
    expect(find.text('no runs (30d)'), findsOneWidget);

    // Revoked tokens (#829): since when the agent's tokens are valid.
    expect(find.bySemanticsLabel('agent-tokens-since:scribe'), findsOneWidget);
    expect(
      find.text('tokens valid since 2026-09-22T10:00:00Z'),
      findsOneWidget,
    );
    // The probe (scripts/verify-demo.sh) reads the visible text off this one node, as its value.
    expect(
      tester
          .getSemantics(find.bySemanticsLabel('agent-tokens-since:scribe'))
          .getSemanticsData()
          .value,
      'tokens valid since 2026-09-22T10:00:00Z',
    );
    expect(find.bySemanticsLabel('agent-tokens-since:janitor'), findsNothing);
  });

  testWidgets('a non-admin sees the profiles but not the usage', (
    tester,
  ) async {
    await _pump(tester, _AgentsClient(isAdmin: false));
    expect(find.bySemanticsLabel('agent-row:scribe'), findsOneWidget);
    expect(find.text('usage: admin only'), findsNWidgets(2));
    expect(find.textContaining('error'), findsNothing);
  });

  testWidgets('no profiles says so', (tester) async {
    await _pump(tester, _AgentsClient(isAdmin: true, agents: const {}));
    expect(find.bySemanticsLabel('agents-panel'), findsOneWidget);
    expect(find.text('no agent profiles'), findsOneWidget);
  });
}
