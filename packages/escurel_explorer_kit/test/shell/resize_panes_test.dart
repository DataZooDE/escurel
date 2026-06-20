// Widget test for the editor shell's drag-resizable side panes. Pumps the
// full EscurelExplorer over a fixture client at a desktop width (>=900, so
// the Row layout is used) and drags each divider, asserting both the
// backing width provider and the rendered catalogue geometry follow.

@TestOn('vm')
library;

import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/escurel_explorer.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: const {
    'note.md':
        '---\ntype: skill\nid: note\ndescription: A note.\n---\n\n# note',
  },
  instanceFiles: const {
    'note__hallo.md':
        '---\ntype: instance\nskill: note\nid: hallo\ntitle: Hallo\n---\n\n# Hallo',
  },
);

Future<void> _pump(WidgetTester tester) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(home: EscurelExplorer(client: _client())),
  );
  await tester.pumpAndSettle();
}

/// The explorer's own provider container (it mounts its own scope).
ProviderContainer _container(WidgetTester tester) =>
    ProviderScope.containerOf(tester.element(find.byType(CataloguePane)));

void main() {
  testWidgets('dragging the left divider widens the catalogue pane', (
    tester,
  ) async {
    await _pump(tester);
    final c = _container(tester);
    expect(c.read(leftPaneWidthProvider), 280);
    expect(tester.getSize(find.byType(CataloguePane)).width, 280);

    await tester.drag(
      find.bySemanticsLabel('resize-left'),
      const Offset(120, 0),
    );
    await tester.pumpAndSettle();

    expect(c.read(leftPaneWidthProvider), 400);
    // The pane actually re-rendered at the new width.
    expect(tester.getSize(find.byType(CataloguePane)).width, 400);
  });

  testWidgets('dragging the right divider widens the right rail', (
    tester,
  ) async {
    await _pump(tester);
    final c = _container(tester);
    expect(c.read(rightPaneWidthProvider), 340);

    // Drag the divider left → the right pane (to its right) grows.
    await tester.drag(
      find.bySemanticsLabel('resize-right'),
      const Offset(-100, 0),
    );
    await tester.pumpAndSettle();

    expect(c.read(rightPaneWidthProvider), 440);
  });

  testWidgets('the left pane width is clamped to a minimum', (tester) async {
    await _pump(tester);
    final c = _container(tester);
    // Drag far left, well past the 180px floor.
    await tester.drag(
      find.bySemanticsLabel('resize-left'),
      const Offset(-600, 0),
    );
    await tester.pumpAndSettle();
    expect(c.read(leftPaneWidthProvider), 180);
    expect(tester.getSize(find.byType(CataloguePane)).width, 180);
  });
}
