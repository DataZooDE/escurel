// No-mock test for hiding archived instances in the catalogue. herkules
// writes `archived: true` in an instance's frontmatter when it is archived
// (recoverable, unlike the erased tombstone). The catalogue hides archived
// rows per skill by default; a per-skill toggle reveals them (muted). Real
// FixtureEscurelClient over a tiny in-memory corpus.

import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _ausgabeSkill =
    '---\ntype: skill\nid: ausgabe\n'
    'description: Notebook outputs.\n---\n# ausgabe\n';
const _live =
    '---\ntype: instance\nskill: ausgabe\nid: li-clean-1\n---\n# li-clean-1\n';
// An archived instance, exactly as herkules leaves it.
const _archived =
    '---\ntype: instance\nskill: ausgabe\nid: li-dbg-1\n'
    'archived: true\n---\n# li-dbg-1\n';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: {'ausgabe.md': _ausgabeSkill},
  instanceFiles: {
    'ausgabe__li-clean-1.md': _live,
    'ausgabe__li-dbg-1.md': _archived,
  },
);

void main() {
  test('InstanceSummary.archived keys on archived: true', () {
    const live = InstanceSummary(
      id: 'ausgabe__li-clean-1',
      skill: 'ausgabe',
      frontmatter: {'id': 'li-clean-1'},
    );
    const archivedBool = InstanceSummary(
      id: 'ausgabe__li-dbg-1',
      skill: 'ausgabe',
      frontmatter: {'id': 'li-dbg-1', 'archived': true},
    );
    const archivedStr = InstanceSummary(
      id: 'ausgabe__li-old',
      skill: 'ausgabe',
      frontmatter: {'id': 'li-old', 'archived': 'true'},
    );
    expect(live.archived, isFalse);
    expect(archivedBool.archived, isTrue);
    expect(archivedStr.archived, isTrue, reason: 'tolerates string "true"');
  });

  testWidgets(
    'CataloguePane hides archived per skill by default; toggle reveals them',
    (tester) async {
      tester.view.physicalSize = const Size(900, 1200);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      final container = ProviderContainer(
        overrides: [escurelClientProvider.overrideWithValue(_client())],
      );
      addTearDown(container.dispose);

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: container,
          child: const MaterialApp(home: Scaffold(body: CataloguePane())),
        ),
      );
      await tester.pumpAndSettle();

      // The live instance is visible; the archived one is hidden by default.
      expect(find.bySemanticsLabel('catalogue-instance:li-clean-1'),
          findsOneWidget);
      expect(find.bySemanticsLabel('catalogue-instance:li-dbg-1'), findsNothing);

      // The per-skill toggle is present (one archived instance).
      final toggle = find.bySemanticsLabel('show-archived-toggle:ausgabe');
      expect(toggle, findsOneWidget);

      // Revealing shows the archived instance.
      await tester.tap(toggle);
      await tester.pumpAndSettle();
      expect(
        find.bySemanticsLabel('catalogue-instance:li-dbg-1'),
        findsOneWidget,
        reason: 'archived instance appears once the skill is toggled on',
      );

      // Hiding again removes it.
      await tester.tap(find.bySemanticsLabel('show-archived-toggle:ausgabe'));
      await tester.pumpAndSettle();
      expect(find.bySemanticsLabel('catalogue-instance:li-dbg-1'), findsNothing);
    },
  );
}
