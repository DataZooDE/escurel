// The catalogue surfaces each skill's stability layer at a glance
// (REQ-LAYER-04): a skill imported from a subscribed pack
// (`layer: base@<pack>@<version>`) carries a LayerBadge with the stable
// `skill-layer:<id>` semantics label (the rodney selector contract) and
// the pack pin as its text; a tenant-authored overlay skill carries
// none.

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

SkillSummary _skill(String id, {String layer = 'overlay', String? shadows}) =>
    SkillSummary(
      id: id,
      description: '',
      requiredFrontmatter: const [],
      optionalFrontmatter: const [],
      layer: layer,
      shadows: shadows,
    );

Future<void> _pump(WidgetTester tester, List<SkillSummary> skills) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        skillsCatalogueProvider.overrideWith((ref) async => skills),
        // The tile watches per-skill instances; keep them empty.
        instancesProvider.overrideWith((ref, id) async => const []),
      ],
      child: const MaterialApp(home: Scaffold(body: CataloguePane())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('base-layer skills carry a layer badge with the pack pin', (
    tester,
  ) async {
    await _pump(tester, [
      _skill('local-notes'),
      _skill('pallet-consolidation', layer: 'base@logistics-midmarket@v7'),
    ]);

    expect(
      find.bySemanticsLabel('skill-layer:pallet-consolidation'),
      findsOneWidget,
    );
    // The badge text is the pin without the `base@` prefix.
    expect(find.text('logistics-midmarket@v7'), findsOneWidget);
    // The tenant-authored overlay skill is unremarkable — no badge.
    expect(find.bySemanticsLabel('skill-layer:local-notes'), findsNothing);
  });

  testWidgets('a shadowing overlay carries the shadows pin', (tester) async {
    await _pump(tester, [
      _skill('local-notes'),
      _skill('pallet-consolidation', shadows: 'base@logistics-midmarket@v7'),
    ]);
    expect(
      find.bySemanticsLabel('skill-shadow:pallet-consolidation'),
      findsOneWidget,
    );
    expect(find.text('shadows logistics-midmarket@v7'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-shadow:local-notes'), findsNothing);
  });
}
