// Widget test for the M7 event-type (SOURCES) filter at the top of the
// event pane. The filter chips are the distinct `label_skill`s across the
// focused instance's event history; selecting one narrows the event list
// to that processing skill. (Each label_skill is the event's link to the
// skill that knows how to process it — NC1.)

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/event_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Event _ev(String id, String source, String labelSkill, String at) => Event(
      eventId: id,
      at: at,
      source: source,
      mime: 'text/plain',
      labelSkill: labelSkill,
      instancePageId: 'spine',
      status: 'processed',
      title: '$source event $id',
      body: id,
      provenance: const {},
    );

class _StubClient implements EscurelClient {
  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async => [
        _ev('e1', 'gmail', 'gmail', '2026-01-01T00:00:00Z'),
        _ev('e2', 'meet', 'meet', '2026-02-01T00:00:00Z'),
        _ev('e3', 'gmail', 'gmail', '2026-03-01T00:00:00Z'),
      ];

  @override
  Future<List<Event>> listInbox({int? limit}) async => const [];

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

Future<void> _pump(WidgetTester tester) async {
  tester.view.physicalSize = const Size(500, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => 'spine'),
      ],
      child: const MaterialApp(home: Scaffold(body: EventPane())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders a sources filter with one chip per distinct label_skill', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('sources-filter'), findsOneWidget);
    // Two distinct processing skills: gmail, meet.
    expect(find.bySemanticsLabel('source-chip:gmail'), findsOneWidget);
    expect(find.bySemanticsLabel('source-chip:meet'), findsOneWidget);
    // All three events show before any filter.
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(3));
  });

  testWidgets('selecting a source narrows the event list to that skill', (tester) async {
    await _pump(tester);
    await tester.tap(find.bySemanticsLabel('source-chip:gmail'));
    await tester.pumpAndSettle();
    // Only the two gmail events remain.
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(2));
    // Tapping it again clears the filter (all three back).
    await tester.tap(find.bySemanticsLabel('source-chip:gmail'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(3));
  });
}
