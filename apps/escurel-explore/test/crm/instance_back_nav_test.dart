// No-mock widget test for the instance-view Back affordance over the real
// crm-demo corpus: following an instance link records a back-stack; the
// Back bar appears and returns focus to where you came from.

@TestOn('vm')
library;

import 'package:escurel_explore/crm/instance_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/crm_demo.dart';

const _weber = 'contact__weber';

void main() {
  testWidgets('back bar appears after following a link and returns on tap', (tester) async {
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

    late WidgetRef capturedRef;
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: MaterialApp(
          home: Scaffold(
            body: SizedBox(
              height: 900,
              child: Consumer(builder: (c, ref, _) {
                capturedRef = ref;
                return const InstancePane();
              }),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // No trail yet → no Back affordance.
    expect(find.bySemanticsLabel('instance-back'), findsNothing);

    // Follow a link to the weber contact (records the spine on the stack).
    navigateToInstance(capturedRef, _weber);
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), _weber);
    expect(find.bySemanticsLabel('instance-back'), findsOneWidget);

    // Back → return to the spine; the affordance disappears again.
    await tester.tap(find.bySemanticsLabel('instance-back'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), crmDemoSpineId);
    expect(container.read(navBackStackProvider), isEmpty);
    expect(find.bySemanticsLabel('instance-back'), findsNothing);
  });
}
