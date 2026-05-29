// Widget test for the CRM workspace shell (PR-3): breadcrumb +
// 3-region body + command bar, backed by a small fixture corpus so it
// renders end-to-end under `flutter test`.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/fixture_escurel_client.dart';
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

Future<void> _pump(WidgetTester tester) async {
  // Wide enough for the three-column layout (>=1000).
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
  testWidgets('renders the data-zoo / CRM breadcrumb', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('brand'), findsOneWidget);
    expect(find.bySemanticsLabel('instances'), findsOneWidget);
  });

  testWidgets('breadcrumb shows the live instance count', (tester) async {
    await _pump(tester);
    // One instance (customer::acme) in the fixture corpus.
    expect(find.textContaining('Instances 1'), findsOneWidget);
  });

  testWidgets('renders the three workspace regions + command bar', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('region-navigator'), findsOneWidget);
    expect(find.bySemanticsLabel('region-entity'), findsOneWidget);
    // Right detail region only at wide widths; the test surface is wide.
    expect(find.bySemanticsLabel('region-detail'), findsOneWidget);
    expect(find.bySemanticsLabel('command-input'), findsOneWidget);
    expect(find.bySemanticsLabel('command-send'), findsOneWidget);
  });
}
