// Widget test for the RBAC group-members admin pane: enter a group +
// subject, tap Add, and assert the new member surfaces in the list.
// Backed by the in-memory fixture client so it runs under `flutter test`.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/crm/group_members_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

EscurelClient _corpus() => FixtureEscurelClient.fromSources(
  skillFiles: const {},
  instanceFiles: const {},
);

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: GroupMembersPane())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders the group-members admin pane', (tester) async {
    await _pump(tester, _corpus());
    expect(find.bySemanticsLabel('group-members-pane'), findsOneWidget);
    expect(find.bySemanticsLabel('group-members-group-field'), findsOneWidget);
    expect(find.bySemanticsLabel('group-member-add'), findsOneWidget);
  });

  testWidgets('adding a subject surfaces it in the member list', (
    tester,
  ) async {
    await _pump(tester, _corpus());

    await tester.enterText(
      find.bySemanticsLabel('group-members-group-field'),
      'editors',
    );
    await tester.enterText(
      find.bySemanticsLabel('group-member-subject-field'),
      'alice',
    );
    await tester.tap(find.bySemanticsLabel('group-member-add'));
    await tester.pumpAndSettle();

    // The new member appears as a chip in the list.
    expect(find.bySemanticsLabel('group-members-list'), findsOneWidget);
    expect(find.text('alice'), findsOneWidget);
  });
}
