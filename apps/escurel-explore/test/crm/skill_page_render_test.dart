// Diagnostic: does the instance pane render a *skill* page (what the ☰
// menu navigates to) without throwing? Over the real corpus.

@TestOn('vm')
library;

import 'package:escurel_explore/crm/instance_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

void main() {
  testWidgets('the instance pane renders a skill page without throwing', (tester) async {
    tester.view.physicalSize = const Size(700, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(crmDemoClient()),
        // The fixture's skill page id is the bare skill id.
        currentPageIdProvider.overrideWith((ref) => 'customer'),
      ],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(home: Scaffold(body: SizedBox(height: 1100, child: InstancePane()))),
      ),
    );
    await tester.pumpAndSettle();

    expect(tester.takeException(), isNull, reason: 'rendering a skill page must not throw');
  });
}
