// Widget tests for the shadow-drift section of a shadowing overlay skill
// page. The ShadowPane is provider-free — it renders straight from an
// ExpandResult — so each case pumps a hand-built page and asserts the
// stable `shadow-pane` semantics label (the rodney selector contract),
// the pack pin, and the per-field drift marking.

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/editor/shadow_pane.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

ExpandResult _page({
  required Map<String, dynamic> frontmatter,
  ShadowInfo? shadow,
}) => ExpandResult(
  pageId: 'markdown/skills/pallet-consolidation.md',
  skill: 'pallet-consolidation',
  pageType: PageType.skill,
  frontmatter: frontmatter,
  body: '',
  blocks: const [],
  wikilinksOut: const [],
  shadow: shadow,
);

Future<void> _pump(WidgetTester tester, ExpandResult page) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: SingleChildScrollView(child: ShadowPane(page: page)),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('a page without a shadow renders nothing', (tester) async {
    await _pump(
      tester,
      _page(frontmatter: const {'description': 'plain overlay skill'}),
    );
    expect(find.byType(ShadowPane), findsOneWidget);
    expect(find.bySemanticsLabel('shadow-pane'), findsNothing);
    expect(find.byKey(const ValueKey('entity_editor.shadow')), findsNothing);
  });

  testWidgets('a shadowing overlay renders the pack pin + drift-marked fields', (
    tester,
  ) async {
    final page = _page(
      frontmatter: const {
        'id': 'pallet-consolidation',
        // Overrides the base value → drift.
        'description': 'Acme-specialised procedure.',
        // Matches the base value → no drift.
        'severity_threshold': 10,
      },
      shadow: const ShadowInfo(
        basePageId:
            'markdown/base/logistics-midmarket/skills/pallet-consolidation.md',
        pack: 'base@logistics-midmarket@v7',
        base: {
          'id': 'pallet-consolidation',
          'description': 'Firm-authored canonical procedure (v1).',
          'severity_threshold': 10,
          // Base-only field (overlay does not set it) → no drift mark.
          'review_cycle_days': 90,
        },
      ),
    );
    await _pump(tester, page);

    expect(find.bySemanticsLabel('shadow-pane'), findsOneWidget);
    // The shadowed pack pin is shown.
    expect(find.textContaining('logistics-midmarket@v7'), findsOneWidget);
    // Every base frontmatter field is listed.
    expect(find.text('description'), findsOneWidget);
    expect(find.text('severity_threshold'), findsOneWidget);
    expect(find.text('review_cycle_days'), findsOneWidget);
    // The base values stay visible (never silently masked).
    expect(
      find.text('Firm-authored canonical procedure (v1).'),
      findsOneWidget,
    );
    // Exactly one field drifts: the overlay overrides `description`.
    expect(find.bySemanticsLabel('shadow-drift:description'), findsOneWidget);
    expect(
      find.bySemanticsLabel('shadow-drift:severity_threshold'),
      findsNothing,
    );
    expect(
      find.bySemanticsLabel('shadow-drift:review_cycle_days'),
      findsNothing,
    );
    // The drifted overlay value is shown alongside the base value.
    expect(find.textContaining('Acme-specialised procedure.'), findsOneWidget);
  });

  testWidgets('non-scalar values compare canonically and render as sorted '
      'JSON', (tester) async {
    final page = _page(
      frontmatter: const {
        // Same map content as the base, different key order — must
        // NOT read as drift (canonical, key-sorted comparison).
        'routing': {'b': 1, 'a': 2},
        // A genuinely different list — real drift.
        'tags': ['crm', 'demo'],
      },
      shadow: const ShadowInfo(
        basePageId: 'base/crm-essentials/skills/playbook.md',
        pack: 'base@crm-essentials@v1',
        base: {
          'routing': {'a': 2, 'b': 1},
          'tags': ['crm', 'essentials'],
        },
      ),
    );
    await _pump(tester, page);

    // Key order is not drift.
    expect(find.bySemanticsLabel('shadow-drift:routing'), findsNothing);
    // The base cell renders canonical JSON, not a Dart debug string.
    expect(find.text('{"a":2,"b":1}'), findsOneWidget);
    expect(find.textContaining('{a: 2, b: 1}'), findsNothing);
    // A truly-different list IS drift, both sides rendered canonically.
    expect(find.bySemanticsLabel('shadow-drift:tags'), findsOneWidget);
    expect(find.text('["crm","essentials"]'), findsOneWidget);
    expect(find.textContaining('["crm","demo"]'), findsOneWidget);
  });
}
