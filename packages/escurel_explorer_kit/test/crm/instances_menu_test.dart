// No-mock widget test for the Instances crumb dropdown over the real
// crm-demo corpus: opening it lists all instances grouped by skill;
// clicking one re-centres the workspace.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/crm/crm_breadcrumb.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  testWidgets('the Instances crumb lists instances grouped by skill and opens one', (tester) async {
    tester.view.physicalSize = const Size(1200, 1000);
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
        child: const MaterialApp(home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink())),
      ),
    );
    await tester.pumpAndSettle();

    // The crumb renders; the list is closed.
    expect(find.bySemanticsLabel('instances'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-row:muenchner-pharma'), findsNothing);

    // Open it → real per-skill groups (multi-account). Groups are sorted,
    // so change_order/contact sit at the top of the (scrollable) panel.
    await tester.tap(find.bySemanticsLabel('instances'));
    await tester.pumpAndSettle();
    expect(find.text('CHANGE_ORDER · 1'), findsOneWidget);
    expect(find.text('CONTACT · 8'), findsOneWidget);

    // Click the first group's row → opens it (by its page id), menu closes.
    expect(find.bySemanticsLabel('instance-row:hoffmann-3site'), findsOneWidget);
    await tester.tap(find.bySemanticsLabel('instance-row:hoffmann-3site'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), 'change_order__hoffmann-3site');
    expect(find.bySemanticsLabel('instance-row:hoffmann-3site'), findsNothing);
  });
}
