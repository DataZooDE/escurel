// Widget test for the M7 CRM workspace shell: breadcrumb + search-top +
// a resizable/collapsible two-pane split (event view left, instance view
// right) + time scrubber + capture-bottom, backed by a small fixture
// corpus so it renders end-to-end under `flutter test`.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/fixture_escurel_client.dart';
import 'package:escurel_explore/crm/crm_providers.dart';
import 'package:escurel_explore/crm/crm_workspace.dart';
import 'package:escurel_explore/state/providers.dart';
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

Future<void> _pump(WidgetTester tester, {List<Override> overrides = const []}) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(_corpus()), ...overrides],
      child: const MaterialApp(home: CrmWorkspace()),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders the data-zoo / CRM breadcrumb', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('brand'), findsOneWidget);
    expect(find.bySemanticsLabel('instances'), findsOneWidget);
  });

  testWidgets('search pinned top, capture pinned bottom', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('search-input'), findsOneWidget);
    expect(find.bySemanticsLabel('search-send'), findsOneWidget);
    expect(find.bySemanticsLabel('capture-input'), findsOneWidget);
    expect(find.bySemanticsLabel('capture-send'), findsOneWidget);
  });

  testWidgets('renders both views of one memory (event left, instance right)', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('region-events'), findsOneWidget);
    expect(find.bySemanticsLabel('region-instance'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-pane'), findsOneWidget);
    // The left view carries the event history + the inbox below it.
    expect(find.bySemanticsLabel('event-history'), findsOneWidget);
    expect(find.bySemanticsLabel('inbox'), findsOneWidget);
    // A draggable divider sits between the two views.
    expect(find.bySemanticsLabel('pane-resize'), findsOneWidget);
  });

  testWidgets('collapsing the left view hides it and offers an expand toggle', (tester) async {
    await _pump(tester, overrides: [leftCollapsedProvider.overrideWith((ref) => true)]);
    // Collapsed: the event pane is gone, an expand toggle remains.
    expect(find.bySemanticsLabel('event-pane'), findsNothing);
    expect(find.bySemanticsLabel('region-events-expand'), findsOneWidget);
    // The instance view still renders.
    expect(find.bySemanticsLabel('instance-pane'), findsOneWidget);
  });
}
