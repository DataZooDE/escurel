// Widget test for the time scrubber (PR-8): given a corpus with a dated
// artifact span, the scrubber renders the readout + speed chips, and
// dragging the slider sets the global `asOfProvider`.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/time_scrubber.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

SkillSummary _skill(String id) => SkillSummary(
      id: id,
      description: id,
      requiredFrontmatter: const [],
      optionalFrontmatter: const [],
    );

class _StubClient implements EscurelClient {
  @override
  Future<List<SkillSummary>> listSkills() async => [_skill('email'), _skill('meeting')];

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
    String? scenario,
  }) async {
    if (skillId == 'email') {
      return const [
        InstanceSummary(
          id: 'a',
          skill: 'email',
          frontmatter: {'at': '2026-01-01T00:00:00Z'},
        ),
        InstanceSummary(
          id: 'b',
          skill: 'email',
          frontmatter: {'at': '2026-03-01T00:00:00Z'},
        ),
      ];
    }
    return const [];
  }

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
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
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
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
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
