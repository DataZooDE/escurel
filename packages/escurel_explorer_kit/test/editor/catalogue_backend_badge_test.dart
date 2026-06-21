// The catalogue surfaces each skill's external backend at a glance: a
// `sql_view` / `document` skill carries a BackendBadge with the stable
// `skill-backend:<id>` semantics label (the rodney selector contract);
// a native markdown skill carries none.

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/catalogue_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

SkillSummary _skill(String id, String kind, SkillCapabilities caps) =>
    SkillSummary(
      id: id,
      description: '',
      requiredFrontmatter: const [],
      optionalFrontmatter: const [],
      backendKind: kind,
      capabilities: caps,
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
  testWidgets('sql_view + document skills carry a backend badge', (
    tester,
  ) async {
    await _pump(tester, [
      _skill('note', 'markdown', const SkillCapabilities()),
      _skill(
        'erp_customer',
        'sql_view',
        const SkillCapabilities(writable: false),
      ),
      _skill('contract', 'document', const SkillCapabilities(writable: false)),
    ]);

    expect(find.bySemanticsLabel('skill-backend:erp_customer'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-backend:contract'), findsOneWidget);
    // The native markdown skill is unremarkable — no backend badge.
    expect(find.bySemanticsLabel('skill-backend:note'), findsNothing);
    expect(find.text('sql view'), findsOneWidget);
    expect(find.text('document'), findsOneWidget);
  });
}
