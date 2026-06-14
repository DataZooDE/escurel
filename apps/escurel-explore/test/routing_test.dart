import 'package:escurel_explore/app.dart';
import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explore/routing/router.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';

EscurelClient _buildCorpus() {
  return FixtureEscurelClient.fromSources(
    skillFiles: const {
      'customer.md': '''---
type: skill
id: customer
description: A buying organisation.
---

# customer
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
''',
    },
  );
}

Widget _appWith(EscurelClient client, {String? initialLocation}) {
  return ProviderScope(
    overrides: [
      escurelClientProvider.overrideWithValue(client),
      if (initialLocation != null)
        routerProvider.overrideWith(
          (ref) => GoRouter(initialLocation: initialLocation, routes: appRoutes),
        ),
    ],
    child: const EscurelExploreApp(),
  );
}

void main() {
  testWidgets('/p/:pageId deep link opens the page in the editor', (tester) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client, initialLocation: '/p/customer__acme'));
    await tester.pumpAndSettle();

    expect(find.text('Acme Ltd'), findsWidgets);
    expect(find.text('customer__acme'), findsWidgets);
  });

  testWidgets('/inspector navigates to the dev inspector shell', (tester) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client, initialLocation: '/inspector'));
    await tester.pumpAndSettle();

    expect(find.text('Dev Inspector — Markdown'), findsOneWidget);
    expect(find.byKey(const ValueKey('md_inspector.input')), findsOneWidget);
    expect(find.byKey(const ValueKey('md_inspector.output')), findsOneWidget);
  });

  testWidgets('topbar inspector toggle navigates between editor and inspector', (tester) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client));
    await tester.pumpAndSettle();

    // Start in editor — the topbar toggle opens the inspector.
    await tester.tap(find.byKey(const ValueKey('topbar.inspector_toggle')));
    await tester.pumpAndSettle();
    expect(find.text('Dev Inspector — Markdown'), findsOneWidget);

    // The inspector's own Back-to-editor button returns us to the editor.
    await tester.tap(find.byTooltip('Back to editor'));
    await tester.pumpAndSettle();
    expect(find.text('Dev Inspector — Markdown'), findsNothing);
  });

  testWidgets('md inspector renders parsed structure for valid input', (tester) async {
    tester.view.physicalSize = const Size(1600, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final client = _buildCorpus();
    addTearDown(client.close);

    await tester.pumpWidget(_appWith(client, initialLocation: '/inspector'));
    await tester.pumpAndSettle();

    // The seed sample already includes wikilinks; output should show them.
    expect(find.byKey(const ValueKey('md_inspector.output')), findsOneWidget);
    expect(find.textContaining('Outgoing wikilinks'), findsOneWidget);
    // Appears in both the input TextField and the parsed-output chip.
    expect(find.textContaining('[[customer::muenchner-pharma]]'), findsAtLeastNWidgets(1));
  });
}
