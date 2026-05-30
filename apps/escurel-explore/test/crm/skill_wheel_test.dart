// No-mock widget test for the radial skill-wheel + lineage rail over the
// real crm-demo corpus: with the richly-connected spine focused, the
// wheel renders a node per unique typed neighbour and the rail a tile per
// link.

@TestOn('vm')
library;

import 'package:escurel_explore/crm/lineage_rail.dart';
import 'package:escurel_explore/crm/skill_wheel.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

Future<void> _pump(WidgetTester tester, Widget child) async {
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        escurelClientProvider.overrideWithValue(crmDemoClient()),
        currentPageIdProvider.overrideWith((ref) => crmDemoSpineId),
      ],
      child: MaterialApp(home: Scaffold(body: child)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('skill-wheel renders a node per unique typed neighbour', (tester) async {
    await _pump(tester, const SkillWheel());
    expect(find.bySemanticsLabel('skill-wheel'), findsOneWidget);
    // The spine connects to many entities (lead, opp, project, contacts,
    // customer, change_order, renewal, …).
    expect(find.bySemanticsLabel('wheel-node'), findsAtLeastNWidgets(6));
    expect(find.textContaining('links'), findsOneWidget);
  });

  testWidgets('lineage rail lists the typed links', (tester) async {
    await _pump(tester, const LineageRail());
    expect(find.bySemanticsLabel('lineage-rail'), findsOneWidget);
    expect(find.bySemanticsLabel('lineage-link'), findsAtLeastNWidgets(6));
  });
}
