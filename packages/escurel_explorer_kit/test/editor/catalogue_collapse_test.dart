// "Collapsing the skill tile must work": the header's custom InkWell focuses
// the skill page, so collapse needs its own reliable control. It must also
// survive the 15s auto-refresh (collapsed-state lives in a provider, not in
// transient ExpansionTile state).
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _skill =
    '---\ntype: skill\nid: ausgabe\ndescription: x\n---\n# ausgabe\n';
const _inst =
    '---\ntype: instance\nskill: ausgabe\nid: li-clean-1\n---\n# li-clean-1\n';

Widget _host(ProviderContainer c) => UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: CataloguePane())),
    );

void main() {
  testWidgets('chevron toggles the skill instance list, and survives refresh',
      (tester) async {
    tester.view.physicalSize = const Size(900, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(overrides: [
      escurelClientProvider.overrideWithValue(
        FixtureEscurelClient.fromSources(
          skillFiles: {'ausgabe.md': _skill},
          instanceFiles: {'ausgabe__li-clean-1.md': _inst},
        ),
      ),
    ]);
    addTearDown(container.dispose);

    await tester.pumpWidget(_host(container));
    await tester.pumpAndSettle();

    final inst = find.bySemanticsLabel('catalogue-instance:li-clean-1');
    final toggle = find.bySemanticsLabel('collapse-toggle:ausgabe');

    expect(inst, findsOneWidget, reason: 'expanded by default');
    expect(toggle, findsOneWidget, reason: 'a reliable collapse control exists');

    // Collapse.
    await tester.tap(toggle);
    await tester.pumpAndSettle();
    expect(inst, findsNothing, reason: 'collapsed → instances hidden');

    // The 15s auto-refresh invalidates the catalogue — collapse must persist.
    container.invalidate(skillsCatalogueProvider);
    await tester.pump();
    await tester.pumpAndSettle();
    expect(inst, findsNothing, reason: 'still collapsed after refresh');

    // Expand again.
    await tester.tap(find.bySemanticsLabel('collapse-toggle:ausgabe'));
    await tester.pumpAndSettle();
    expect(inst, findsOneWidget, reason: 're-expanded');
  });
}
