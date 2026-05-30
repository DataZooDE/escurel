// Widget test for the time scrubber (M7): given a focused instance with a
// dated event span, the scrubber renders the readout + speed chips, and
// dragging the slider sets the global `asOfProvider`. The corpus range now
// derives from the focused instance's event history (`list_events`).

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/time_scrubber.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Event _event(String id, String at) => Event(
      eventId: id,
      at: at,
      source: 'gmail',
      mime: 'message/rfc822',
      labelSkill: 'gmail',
      instancePageId: 'spine',
      status: 'processed',
      title: id,
      body: id,
      provenance: const {},
    );

class _StubClient implements EscurelClient {
  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async => [
        _event('a', '2026-01-01T00:00:00Z'),
        _event('b', '2026-03-01T00:00:00Z'),
      ];

  @override
  Future<List<Event>> listInbox({int? limit}) async => const [];

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

Future<void> _pump(WidgetTester tester, ProviderContainer container) async {
  tester.view.physicalSize = const Size(1200, 200);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(home: Scaffold(body: TimeScrubber())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders readout + speed chips, defaults to the present', (tester) async {
    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => 'spine'),
      ],
    );
    addTearDown(container.dispose);
    await _pump(tester, container);

    expect(find.bySemanticsLabel('time-scrubber'), findsOneWidget);
    expect(find.bySemanticsLabel('time-readout'), findsOneWidget);
    expect(find.bySemanticsLabel('speed-1x'), findsOneWidget);
    expect(find.bySemanticsLabel('speed-500x'), findsOneWidget);
    // No cut yet → present.
    expect(container.read(asOfProvider), isNull);
    expect(find.text('now'), findsOneWidget);
  });

  testWidgets('dragging the slider left sets a past as_of cut', (tester) async {
    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => 'spine'),
      ],
    );
    addTearDown(container.dispose);
    await _pump(tester, container);

    await tester.drag(find.byType(Slider), const Offset(-300, 0));
    await tester.pump();

    final asOf = container.read(asOfProvider);
    expect(asOf, isNotNull);
    // The cut lands within the corpus span [Jan 1 .. Mar 1].
    expect(asOf!.isAfter(DateTime.utc(2025, 12, 31)), isTrue);
    expect(asOf.isBefore(DateTime.utc(2026, 3, 2)), isTrue);
  });
}
