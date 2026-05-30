// No-mock widget test for the instance-view "state over time" version
// markers over the real crm-demo corpus: the spine has a 4-state snapshot
// timeline, so a `now` chip plus v1..v4 render; tapping a version sets the
// global asOf cut to that snapshot's taken_at.

@TestOn('vm')
library;

import 'package:escurel_explore/crm/instance_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

Future<ProviderContainer> _pump(WidgetTester tester) async {
  tester.view.physicalSize = const Size(700, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  final container = ProviderContainer(
    overrides: [
      escurelClientProvider.overrideWithValue(crmDemoClient()),
      currentPageIdProvider.overrideWith((ref) => crmDemoSpineId),
    ],
  );
  addTearDown(container.dispose);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(home: Scaffold(body: SizedBox(height: 900, child: InstancePane()))),
    ),
  );
  await tester.pumpAndSettle();
  return container;
}

void main() {
  testWidgets('renders a now chip plus one chip per snapshot', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('version-markers'), findsOneWidget);
    expect(find.bySemanticsLabel('version-now'), findsOneWidget);
    // The spine has 4 snapshots.
    expect(find.bySemanticsLabel('version-v1'), findsOneWidget);
    expect(find.bySemanticsLabel('version-v4'), findsOneWidget);
  });

  testWidgets('tapping a version sets the asOf cut; now clears it', (tester) async {
    final container = await _pump(tester);
    expect(container.read(asOfProvider), isNull, reason: 'starts at now');

    await tester.tap(find.bySemanticsLabel('version-v2'));
    await tester.pumpAndSettle();
    expect(
      container.read(asOfProvider),
      DateTime.parse('2026-03-18T16:00:00Z').toUtc(),
      reason: 'v2 jumps the cut to the second snapshot',
    );

    await tester.tap(find.bySemanticsLabel('version-now'));
    await tester.pumpAndSettle();
    expect(container.read(asOfProvider), isNull, reason: 'now clears the cut');
  });
}
