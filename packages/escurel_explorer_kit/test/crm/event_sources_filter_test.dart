// No-mock widget test for the event-type (SOURCES) filter over the real
// crm-demo corpus. The spine's processed events span three processing
// skills (email ×3, meeting ×2, doc ×1); the filter chips are those
// label_skills, and selecting one narrows the list.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/crm/event_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

Future<void> _pump(WidgetTester tester) async {
  // Tall surface so the events list isn't virtualised away (all 6 render).
  tester.view.physicalSize = const Size(520, 2600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        escurelClientProvider.overrideWithValue(crmDemoClient()),
        currentPageIdProvider.overrideWith((ref) => crmDemoSpineId),
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
    // The spine's three processing skills.
    expect(find.bySemanticsLabel('source-chip:email'), findsOneWidget);
    expect(find.bySemanticsLabel('source-chip:meeting'), findsOneWidget);
    expect(find.bySemanticsLabel('source-chip:doc'), findsOneWidget);
    // All six processed events show before any filter.
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(6));
  });

  testWidgets('selecting a source narrows the event list to that skill', (tester) async {
    await _pump(tester);
    await tester.tap(find.bySemanticsLabel('source-chip:email'));
    await tester.pumpAndSettle();
    // The three email events remain.
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(3));
    // Re-tap clears the filter (all six back).
    await tester.tap(find.bySemanticsLabel('source-chip:email'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('event-item'), findsNWidgets(6));
  });
}
