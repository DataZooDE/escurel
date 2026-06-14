/// Merge-gate widget test for the editor surface.
///
/// Per CLAUDE.md principle 2 ("no-mock integration test"), this is
/// the test that proves the editor renders a real escurel page
/// end-to-end. It uses:
///
/// - Real Flutter widget tree (no mocks of any widget).
/// - Real Dart frontmatter + wikilink parsers.
/// - Real [FixtureEscurelClient] over a real (inline) corpus.
/// - Real Riverpod scope.
///
/// The only seam is the asset bundle — fixture mode bypasses it by
/// reading raw markdown directly. Once PR-7 sets up headless
/// chromium in CI, an integration_test/ variant of this test will
/// run against an `examples/crm-demo/` asset bundle, and another
/// against a real Dockerised escurel-server.
library;

import 'package:escurel_explore/app.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:escurel_explorer_kit/widgets/kind_chip.dart';
import 'package:escurel_explorer_kit/widgets/wikilink_pill.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

EscurelClient _buildCorpus() {
  return FixtureEscurelClient.fromSources(
    skillFiles: const {
      'customer.md': '''---
type: skill
id: customer
description: A buying organisation.
required_frontmatter: [name, country]
---

# customer
''',
      'contact.md': '''---
type: skill
id: contact
description: An individual at a customer.
required_frontmatter: [name, customer]
---

# contact
''',
    },
    instanceFiles: const {
      'customer__acme.md': '''---
type: instance
skill: customer
id: acme
name: Acme Ltd
country: DE
---

# Acme Ltd

Primary champion: [[contact::dora]].
''',
      'contact__dora.md': '''---
type: instance
skill: contact
id: dora
name: Dora Doe
customer: [[customer::acme]]
role: VP Engineering
---

# Dora Doe
''',
    },
  );
}

Widget _appUnderTest(EscurelClient client) {
  return ProviderScope(
    overrides: [escurelClientProvider.overrideWithValue(client)],
    child: const EscurelExploreApp(),
  );
}

void main() {
  testWidgets('editor renders customer__acme with frontmatter, body, wikilink pill, and backlink', (tester) async {
    // Roomy viewport so the wide-screen three-pane layout kicks in.
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appUnderTest(client));
    await tester.pumpAndSettle();

    // Catalogue: the two seeded skills appear.
    expect(find.text('customer'), findsOneWidget);
    expect(find.text('contact'), findsOneWidget);

    // Both skills auto-expand on load, so each instance shortId
    // (everything after `__`) is visible.
    expect(find.text('acme'), findsOneWidget);
    expect(find.text('dora'), findsOneWidget);

    // Click the customer instance — editor opens it.
    await tester.tap(find.text('acme'));
    await tester.pumpAndSettle();

    expect(
      find.byKey(const ValueKey('entity_editor.title')),
      findsOneWidget,
      reason: 'editor should mount a title widget after a page is opened',
    );
    expect(find.text('Acme Ltd'), findsWidgets);

    // Frontmatter table renders the page id and the `country` field.
    expect(find.byKey(const ValueKey('entity_editor.frontmatter')), findsOneWidget);
    expect(find.text('country'), findsOneWidget);
    expect(find.text('DE'), findsOneWidget);
    expect(find.text('customer__acme'), findsWidgets);

    // The body line "Primary champion: [[contact::dora]]" should
    // render the wikilink as a pill, not raw text.
    expect(
      find.byWidgetPredicate((w) => w is RichText && w.text.toPlainText().contains('Primary champion')),
      findsAtLeastNWidgets(1),
    );
    expect(find.byType(WikilinkPill), findsAtLeastNWidgets(1));

    // Backlinks rail surfaces the incoming neighbour from contact__dora.
    expect(find.byKey(const ValueKey('right_rail.backlinks')), findsOneWidget);
    expect(find.text('contact__dora'), findsOneWidget);
  });

  testWidgets('tapping the wikilink pill navigates the editor to the linked page', (tester) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appUnderTest(client));
    await tester.pumpAndSettle();

    // Open customer__acme.
    await tester.tap(find.text('acme'));
    await tester.pumpAndSettle();

    // Tap the only wikilink pill on screen.
    expect(find.byType(WikilinkPill), findsAtLeastNWidgets(1));
    await tester.tap(find.byType(WikilinkPill).first);
    await tester.pumpAndSettle();

    // Editor switched to contact__dora.
    expect(find.text('Dora Doe'), findsWidgets);
    expect(find.text('contact__dora'), findsWidgets);
    expect(find.byType(KindChip), findsAtLeastNWidgets(1));
  });
}
