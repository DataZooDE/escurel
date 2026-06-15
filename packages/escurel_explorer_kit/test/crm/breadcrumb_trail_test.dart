// No-mock test for the breadcrumb history trail. Following links builds a
// back-stack (navBackStackProvider); the breadcrumb renders that stack as
// clickable crumbs (A › B › current), and clicking an ancestor jumps focus
// back to that depth. Complements the instance-pane Back button.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/crm/crm_breadcrumb.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _skill =
    '---\ntype: skill\nid: talk\ndescription: A talk.\n---\n# talk\n';
String _inst(String id) =>
    '---\ntype: instance\nskill: talk\nid: $id\nname: $id\n---\n# $id\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: {'talk.md': _skill},
  instanceFiles: {
    'talk__a.md': _inst('a'),
    'talk__b.md': _inst('b'),
    'talk__c.md': _inst('c'),
  },
);

void main() {
  testWidgets('breadcrumb renders the nav history as clickable crumbs', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1400, 800);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(_client())],
    );
    addTearDown(container.dispose);

    // Simulate having navigated a → b → c.
    container.read(navBackStackProvider.notifier).state = [
      'talk__a',
      'talk__b',
    ];
    container.read(currentPageIdProvider.notifier).state = 'talk__c';

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink()),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // Ancestors render as clickable crumbs; the current page as the focused crumb.
    expect(find.bySemanticsLabel('crumb:a'), findsOneWidget);
    expect(find.bySemanticsLabel('crumb:b'), findsOneWidget);
    expect(find.bySemanticsLabel('focused-entity'), findsOneWidget);

    // Clicking the first ancestor jumps focus back to that depth.
    await tester.tap(find.bySemanticsLabel('crumb:a'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), 'talk__a');
    expect(
      container.read(navBackStackProvider),
      isEmpty,
      reason: 'jumping to depth 0 truncates the stack',
    );
  });
}
