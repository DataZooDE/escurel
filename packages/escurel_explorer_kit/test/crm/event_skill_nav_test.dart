// No-mock widget test over the real crm-demo corpus: shift-clicking an
// event tile routes focus to the event's processing skill (its
// label_skill), uniform with the wikilink pills.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/crm/event_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  testWidgets('shift-click on an event tile navigates to its skill', (tester) async {
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
        child: const MaterialApp(
          home: Scaffold(body: SizedBox(height: 900, child: EventPane())),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // The spine has its real event history; the first (oldest) event is
    // the contact-form email → label_skill `email`.
    expect(find.bySemanticsLabel('event-item'), findsWidgets);

    // Default tap opens the event (does not move focus).
    await tester.tap(find.bySemanticsLabel('event-item').first);
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), crmDemoSpineId, reason: 'plain tap keeps the entity');

    // Shift-click jumps to the email skill manifest.
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.tap(find.bySemanticsLabel('event-item').first);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    await tester.pumpAndSettle();

    expect(container.read(currentPageIdProvider), 'email');
    expect(container.read(navBackStackProvider), contains(crmDemoSpineId));
  });
}
