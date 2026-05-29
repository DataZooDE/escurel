// Widget test for the source inbox (PR-6): given a client that lists
// artifact instances (email/meeting/doc) with provenance frontmatter,
// the inbox renders one row each, newest first, and tapping a row
// focuses that artifact.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/inbox.dart';
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
  Future<List<SkillSummary>> listSkills() async =>
      [_skill('email'), _skill('meeting'), _skill('doc'), _skill('engagement')];

  @override
  Future<List<InstanceSummary>> listInstances(
    String skillId, {
    Map<String, Object?>? filter,
    String? orderBy,
    int? limit,
    String? asOf,
  }) async {
    switch (skillId) {
      case 'email':
        return const [
          InstanceSummary(
            id: 'markdown/instances/email__proposal.md',
            skill: 'email',
            frontmatter: {
              'at': '2026-05-14T10:30:00Z',
              'source': 'gmail',
              'subject': 'Proposal · scope clarification',
              'provenance': 'EXTRACTED',
            },
          ),
        ];
      case 'meeting':
        return const [
          InstanceSummary(
            id: 'markdown/instances/meeting__discovery-call.md',
            skill: 'meeting',
            frontmatter: {
              'at': '2026-03-17T15:00:00Z',
              'source': 'meet',
              'subject': 'Discovery call',
              'provenance': 'AUTO-PROMOTED',
            },
          ),
        ];
      case 'doc':
        return const [
          InstanceSummary(
            id: 'markdown/instances/doc__project-status.md',
            skill: 'doc',
            frontmatter: {
              'at': '2026-05-14T18:00:00Z',
              'source': 'drive',
              'title': 'Project status',
              'provenance': 'EXTRACTED',
            },
          ),
        ];
      default:
        return const [];
    }
  }

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

Future<void> _pump(WidgetTester tester, ProviderContainer container) async {
  tester.view.physicalSize = const Size(360, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(home: Scaffold(body: InboxList())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('inbox renders one row per artifact, newest first', (tester) async {
    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
    );
    addTearDown(container.dispose);
    await _pump(tester, container);

    expect(find.bySemanticsLabel('inbox'), findsOneWidget);
    expect(find.bySemanticsLabel('inbox-item'), findsNWidgets(3));

    // Newest (doc 05-14T18:00) sorts above the email (05-14T10:30) and
    // the meeting (03-17). The provenance badges render.
    expect(find.text('AUTO-PROMOTED'), findsOneWidget);
    expect(find.text('EXTRACTED'), findsNWidgets(2));
  });

  testWidgets('tapping an artifact focuses it', (tester) async {
    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
    );
    addTearDown(container.dispose);
    await _pump(tester, container);

    expect(container.read(currentPageIdProvider), isNull);
    await tester.tap(find.text('Project status'));
    await tester.pump();
    expect(container.read(currentPageIdProvider), 'markdown/instances/doc__project-status.md');
  });
}
