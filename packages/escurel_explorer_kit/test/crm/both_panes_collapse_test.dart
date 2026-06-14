// Reproduction for the live report: on an instance, collapsing BOTH panes
// leaves the expand chevrons unresponsive. Drives the real chevrons via
// the pointer path over the real fixture.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/crm/crm_workspace.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

EscurelClient _corpus() => FixtureEscurelClient.fromSources(
      skillFiles: const {
        'customer.md': '---\ntype: skill\nid: customer\ndescription: A buying org.\n---\n# customer\n',
      },
      instanceFiles: const {
        'customer__acme.md':
            '---\ntype: instance\nskill: customer\nid: acme\nname: Acme Ltd\n---\n# Acme Ltd\n',
      },
    );

Future<void> _pump(WidgetTester tester) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(_corpus())],
      child: const MaterialApp(home: CrmWorkspace()),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('collapse both panes, then both expand chevrons work', (tester) async {
    await _pump(tester);

    // Collapse the left (events) pane.
    await tester.tap(find.bySemanticsLabel('region-events-collapse'));
    await tester.pumpAndSettle();
    // Collapse the right (instance) pane.
    await tester.tap(find.bySemanticsLabel('region-instance-collapse'));
    await tester.pumpAndSettle();

    // Both collapsed: two expand chevrons, no panes.
    expect(find.bySemanticsLabel('region-events-expand'), findsOneWidget);
    expect(find.bySemanticsLabel('region-instance-expand'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsNothing);
    expect(find.bySemanticsLabel('instance-pane'), findsNothing);

    // Expand the right pane.
    await tester.tap(find.bySemanticsLabel('region-instance-expand'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('instance-pane'), findsOneWidget,
        reason: 'right pane must re-expand');

    // Expand the left pane.
    await tester.tap(find.bySemanticsLabel('region-events-expand'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget,
        reason: 'left pane must re-expand');
  });

  testWidgets('a tap anywhere in the collapsed rail re-expands it (not just the icon)', (tester) async {
    await _pump(tester);

    await tester.tap(find.bySemanticsLabel('region-events-collapse'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('event-pane'), findsNothing);

    // Tap low in the collapsed rail — well away from the chevron icon
    // (which sits in the top band). Only a full-rail hit target responds
    // here; the old centered-icon target would have ignored this click.
    final rail = tester.getRect(find.bySemanticsLabel('region-events'));
    await tester.tapAt(Offset(rail.center.dx, rail.top + rail.height * 0.75));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget,
        reason: 'tapping the rail (off the icon) must still expand it');
  });
}
