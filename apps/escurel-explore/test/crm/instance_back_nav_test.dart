// Widget test for the instance-view Back affordance: following an
// instance link records a back-stack; the Back bar appears and returns
// focus to where you came from.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/crm/instance_pane.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _spine = 'markdown/instances/engagement__hoffmann-spine.md';
const _weber = 'markdown/instances/contact__weber.md';

class _StubClient implements EscurelClient {
  @override
  Future<List<String>> listSnapshots(String pageId) async => const [];
  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

void main() {
  testWidgets('back bar appears after following a link and returns on tap', (tester) async {
    tester.view.physicalSize = const Size(700, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => _spine),
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
    expect(container.read(currentPageIdProvider), _spine);
    expect(container.read(navBackStackProvider), isEmpty);
    expect(find.bySemanticsLabel('instance-back'), findsNothing);
  });
}
