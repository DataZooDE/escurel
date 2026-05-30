// Widget test for the M7 instance-view "state over time" version
// markers: one chip per recorded CRDT snapshot plus a `now` chip.
// Tapping a version sets the global asOf cut to that snapshot's taken_at
// (driving expand replay); `now` clears it.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/crm/instance_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _StubClient implements EscurelClient {
  @override
  Future<List<String>> listSnapshots(String pageId) async => const [
        '2026-03-12T14:20:00Z',
        '2026-03-18T16:00:00Z',
        '2026-05-02T09:00:00Z',
      ];

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

Future<ProviderContainer> _pump(WidgetTester tester) async {
  tester.view.physicalSize = const Size(700, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  final container = ProviderContainer(
    overrides: [
      escurelClientProvider.overrideWithValue(_StubClient()),
      currentPageIdProvider.overrideWith((ref) => 'spine'),
    ],
  );
  addTearDown(container.dispose);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      // The markers widget is private; exercise it through InstancePane
      // (in a bounded box so its Expanded editor lays out). The wheel +
      // editor fall into harmless error states under the partial stub —
      // only the marker row matters here.
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
    expect(find.bySemanticsLabel('version-v1'), findsOneWidget);
    expect(find.bySemanticsLabel('version-v2'), findsOneWidget);
    expect(find.bySemanticsLabel('version-v3'), findsOneWidget);
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
