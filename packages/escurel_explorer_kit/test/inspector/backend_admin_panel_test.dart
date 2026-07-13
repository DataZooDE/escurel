// Widget tests for the Backends admin panel. Backed by the in-memory
// fixture client (the real EscurelClient boundary the explorer uses), so
// each action — register credential, validate bindings, create SQL
// instance, ingest a document — runs end-to-end against a live in-process
// implementation, no test doubles. Asserts the stable semantics labels
// (the rodney selector contract) and the round-trip outcomes.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/inspector/backend_admin_panel.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

FixtureEscurelClient _client() => FixtureEscurelClient.fromSources(
  skillFiles: {
    'erp_customer.md':
        '---\ntype: skill\nid: erp_customer\ndescription: ERP.\nbackend:\n  kind: sql_view\n---\n\n# erp_customer',
    'contract.md':
        '---\ntype: skill\nid: contract\ndescription: Docs.\nbackend:\n  kind: document\n---\n\n# contract',
  },
  instanceFiles: const {},
);

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1200, 1600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: BackendAdminPanel())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders the five trigger cards', (tester) async {
    await _pump(tester, _client());
    expect(find.bySemanticsLabel('backend-admin-panel'), findsOneWidget);
    expect(find.bySemanticsLabel('cred-register-button'), findsOneWidget);
    expect(find.bySemanticsLabel('validate-bindings-button'), findsOneWidget);
    expect(find.bySemanticsLabel('create-sql-submit'), findsOneWidget);
    expect(find.bySemanticsLabel('ingest-submit'), findsOneWidget);
    expect(find.bySemanticsLabel('list-packs-button'), findsOneWidget);
  });

  testWidgets('listing packs shows the empty-state for an overlay-only node', (
    tester,
  ) async {
    // The fixture client subscribes to nothing — the honest default:
    // an isolated spoke that runs on its own overlay.
    await _pump(tester, _client());
    await tester.tap(find.bySemanticsLabel('list-packs-button'));
    await tester.pumpAndSettle();
    expect(
      find.textContaining('no packs subscribed'),
      findsOneWidget,
    );
  });

  testWidgets('registering a credential surfaces it in the list', (
    tester,
  ) async {
    await _pump(tester, _client());
    await tester.enterText(find.bySemanticsLabel('cred-name-field'), 'crm_pg');
    await tester.enterText(
      find.bySemanticsLabel('cred-secret-field'),
      'postgres://secret',
    );
    await tester.tap(find.bySemanticsLabel('cred-register-button'));
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('cred-item:crm_pg'), findsOneWidget);
    // The connector is shown; the secret value never comes back.
    expect(find.text('postgres'), findsWidgets);
    expect(find.text('postgres://secret'), findsNothing);

    // Deleting removes it.
    await tester.tap(find.bySemanticsLabel('cred-delete:crm_pg'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('cred-item:crm_pg'), findsNothing);
  });

  testWidgets('creating a SQL-view instance reports the new page id', (
    tester,
  ) async {
    final client = _client();
    await _pump(tester, client);
    await tester.enterText(
      find.bySemanticsLabel('create-sql-id-field'),
      'acme',
    );
    await tester.tap(find.bySemanticsLabel('create-sql-submit'));
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('create-sql-result'), findsOneWidget);
    expect(find.textContaining('erp_customer__acme'), findsOneWidget);
    // The instance actually exists on the client now.
    final page = await client.expand('erp_customer__acme');
    expect(page.backendKind, 'sql_view');
  });

  testWidgets('ingesting a document shows the pipeline outcome', (
    tester,
  ) async {
    await _pump(tester, _client());
    await tester.enterText(
      find.bySemanticsLabel('ingest-title-field'),
      'Q3 contract',
    );
    await tester.enterText(
      find.bySemanticsLabel('ingest-text-field'),
      'First clause.\n\nSecond clause.\n\nThird clause.',
    );
    await tester.tap(find.bySemanticsLabel('ingest-submit'));
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('ingest-outcome'), findsOneWidget);
    expect(find.text('materialised'), findsOneWidget);
    // The handler skill + chunk count are surfaced for debugging.
    expect(find.text('contract'), findsWidgets);
    expect(find.text('3'), findsOneWidget);
  });
}
