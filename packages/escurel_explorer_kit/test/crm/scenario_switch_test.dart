// Widget test for the scenario switch (PR-10): renders Base/A/B/C,
// defaults to Base (null), and tapping a chip sets scenarioProvider.

import 'package:escurel_explorer_kit/crm/scenario_switch.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> _pump(WidgetTester tester, ProviderContainer container) async {
  tester.view.physicalSize = const Size(700, 120);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(home: Scaffold(body: Center(child: ScenarioSwitch()))),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders Base/A/B/C and defaults to base (null)', (tester) async {
    final container = ProviderContainer();
    addTearDown(container.dispose);
    await _pump(tester, container);

    expect(find.bySemanticsLabel('scenario-switch'), findsOneWidget);
    expect(find.bySemanticsLabel('scenario-base'), findsOneWidget);
    expect(find.bySemanticsLabel('scenario-A'), findsOneWidget);
    expect(find.bySemanticsLabel('scenario-B'), findsOneWidget);
    expect(find.bySemanticsLabel('scenario-C'), findsOneWidget);
    expect(container.read(scenarioProvider), isNull);
  });

  testWidgets('tapping B sets the scenario, Base clears it', (tester) async {
    final container = ProviderContainer();
    addTearDown(container.dispose);
    await _pump(tester, container);

    await tester.tap(find.text('B'));
    await tester.pump();
    expect(container.read(scenarioProvider), 'B');

    await tester.tap(find.text('Base'));
    await tester.pump();
    expect(container.read(scenarioProvider), isNull);
  });
}
