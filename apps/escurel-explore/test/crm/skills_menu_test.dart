// No-mock widget test for the ☰ skills registry over the real crm-demo
// corpus: opening it lists skills grouped ENTITY-BOUND / EVENT-TYPED;
// clicking a skill opens its manifest.

@TestOn('vm')
library;

import 'package:escurel_explore/crm/crm_breadcrumb.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  testWidgets('the ☰ menu lists grouped skills and opens one on tap', (tester) async {
    tester.view.physicalSize = const Size(1200, 800);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(crmDemoClient())],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink()),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('skills-menu'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:customer'), findsNothing);

    // Open it → real skills grouped. The ENTITY-BOUND group sits at the
    // top of the (scrollable) panel; the data-level test
    // (fixture_crm_demo_test) covers that both groups exist.
    await tester.tap(find.bySemanticsLabel('skills-menu'));
    await tester.pumpAndSettle();
    expect(find.textContaining('ENTITY-BOUND ·'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:contact'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:customer'), findsOneWidget);

    // Click the customer skill → opens its manifest page, menu closes.
    await tester.tap(find.bySemanticsLabel('skill-row:customer'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), 'customer');
    expect(find.bySemanticsLabel('skill-row:customer'), findsNothing);
  });
}
