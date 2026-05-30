// Integration-ish widget test: shift-clicking an event tile routes the
// focus to the event's processing skill (its label_skill), uniform with
// the wikilink pills. Exercises the full path event tile →
// InstanceSkillLink → focusSkill → resolve('[[meeting]]') →
// navigateToInstance.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/event_pane.dart';
import 'package:escurel_explore/md/frontmatter.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _spine = 'markdown/instances/engagement__hoffmann-spine.md';
const _meetingSkill = 'markdown/skills/meeting.md';

Event _meetingEvent() => const Event(
      eventId: 'ev1',
      at: '2026-03-18T16:00:00Z',
      source: 'meet',
      mime: 'text/vtt',
      labelSkill: 'meeting',
      instancePageId: _spine,
      status: 'processed',
      title: 'Discovery call',
      body: 'transcript',
      provenance: {},
    );

class _StubClient implements EscurelClient {
  @override
  Future<List<Event>> listEvents(String instancePageId, {int? limit}) async => [_meetingEvent()];
  @override
  Future<List<Event>> listInbox({int? limit}) async => const [];
  @override
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) async {
    if (wikilink == '[[meeting]]') {
      return const ResolveResult(
        pageId: _meetingSkill,
        skill: 'meeting',
        pageType: PageType.skill,
        exists: true,
      );
    }
    return const ResolveResult(pageId: '', skill: '', pageType: PageType.instance, exists: false);
  }

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

void main() {
  testWidgets('shift-click on an event tile navigates to its skill', (tester) async {
    tester.view.physicalSize = const Size(700, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => _spine),
      ],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(body: SizedBox(height: 900, child: EventPane())),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('event-item'), findsOneWidget);

    // Default tap opens the event (does not move focus).
    await tester.tap(find.bySemanticsLabel('event-item'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), _spine, reason: 'plain tap keeps the entity');

    // Shift-click jumps to the meeting skill.
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.tap(find.bySemanticsLabel('event-item'));
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    await tester.pumpAndSettle();

    expect(container.read(currentPageIdProvider), _meetingSkill);
    // The back-stack recorded the spine, so Back returns.
    expect(container.read(navBackStackProvider), contains(_spine));
  });
}
