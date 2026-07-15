// Widget tests for the Backends admin panel. Backed by the in-memory
// fixture client (the real EscurelClient boundary the explorer uses), so
// each action — register credential, validate bindings, create SQL
// instance, ingest a document — runs end-to-end against a live in-process
// implementation, no test doubles. Asserts the stable semantics labels
// (the rodney selector contract) and the round-trip outcomes.

import 'dart:async';

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
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

/// A fixture corpus that also carries a base-layer skill from a
/// subscribed pack, so `listPacks` synthesizes a real subscription row.
FixtureEscurelClient _subscribedClient() => FixtureEscurelClient.fromSources(
  skillFiles: {
    'erp_customer.md':
        '---\ntype: skill\nid: erp_customer\ndescription: ERP.\nbackend:\n  kind: sql_view\n---\n\n# erp_customer',
    'base/crm-essentials/skills/playbook.md':
        '---\ntype: skill\nid: playbook\ndescription: Firm playbook.\nlayer: base@crm-essentials@v1\n---\n\n# playbook',
  },
  instanceFiles: const {},
);

/// Delegates the surfaces the panel auto-loads to the fixture, but holds
/// `unsubscribePack` on a gate so the widget test can observe the card's
/// MID-FLIGHT state (armed confirm retained + disabled). Any surface not
/// forwarded below routes to `noSuchMethod` and fails loudly.
class _GatedUnsubscribeClient implements EscurelClient {
  _GatedUnsubscribeClient(this._inner);

  final EscurelClient _inner;
  final Completer<void> gate = Completer<void>();

  @override
  Future<List<SkillSummary>> listSkills() => _inner.listSkills();

  @override
  Future<List<CredentialInfo>> listCredentials() => _inner.listCredentials();

  @override
  Future<List<PackSubscriptionInfo>> listPacks() => _inner.listPacks();

  @override
  Future<PackOpResult> unsubscribePack(String packId) async {
    await gate.future;
    return _inner.unsubscribePack(packId);
  }

  @override
  void close() => _inner.close();

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  // Tall enough that every card (incl. the last, Import pack) stays
  // on-stage — off-screen buttons make `tester.tap` silently miss.
  tester.view.physicalSize = const Size(1200, 2400);
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
  testWidgets('renders the seven trigger cards', (tester) async {
    await _pump(tester, _client());
    expect(find.bySemanticsLabel('backend-admin-panel'), findsOneWidget);
    expect(find.bySemanticsLabel('cred-register-button'), findsOneWidget);
    expect(find.bySemanticsLabel('validate-bindings-button'), findsOneWidget);
    expect(find.bySemanticsLabel('create-sql-submit'), findsOneWidget);
    expect(find.bySemanticsLabel('ingest-submit'), findsOneWidget);
    expect(find.bySemanticsLabel('list-packs-button'), findsOneWidget);
    expect(find.bySemanticsLabel('pack-import-submit'), findsOneWidget);
    expect(find.bySemanticsLabel('endpoint-register-button'), findsOneWidget);
    expect(find.bySemanticsLabel('validate-endpoints-button'), findsOneWidget);
  });

  testWidgets('registering a remote endpoint surfaces it in the list '
      'and never echoes the secret', (tester) async {
    final client = _client();
    await _pump(tester, client);
    // Empty registry → honest empty-state.
    expect(find.textContaining('no endpoints registered'), findsOneWidget);

    await tester.enterText(
      find.bySemanticsLabel('endpoint-name-field'),
      'yahoo_finance',
    );
    await tester.enterText(
      find.bySemanticsLabel('endpoint-url-field'),
      'https://query1.finance.yahoo.com',
    );
    await tester.enterText(
      find.bySemanticsLabel('endpoint-secret-field'),
      'tok-3nt',
    );
    await tester.tap(find.bySemanticsLabel('endpoint-register-button'));
    await tester.pumpAndSettle();

    expect(find.bySemanticsLabel('endpoints-list'), findsOneWidget);
    expect(
      find.bySemanticsLabel('endpoint-item:yahoo_finance'),
      findsOneWidget,
    );
    // Kind + URL are shown; the secret value never comes back.
    expect(
      find.textContaining('https://query1.finance.yahoo.com'),
      findsWidgets,
    );
    expect(find.text('tok-3nt'), findsNothing);
    // The client really holds the registration (a non-empty secret
    // registers bearer auth server-side).
    final eps = await client.listEndpoints();
    expect(eps.single.name, 'yahoo_finance');
    expect(eps.single.kind, 'openapi');
    expect(eps.single.authScheme, 'bearer');
  });

  testWidgets('validating endpoints shows per-endpoint reachability', (
    tester,
  ) async {
    final client = _client();
    await client.registerEndpoint(
      name: 'upstream_kb',
      kind: 'mcp',
      baseUrl: 'http://127.0.0.1:9999/mcp',
    );
    await _pump(tester, client);
    expect(find.bySemanticsLabel('endpoint-item:upstream_kb'), findsOneWidget);

    await tester.tap(find.bySemanticsLabel('validate-endpoints-button'));
    await tester.pumpAndSettle();
    // The fixture registry probes every endpoint reachable; the status
    // lands on the endpoint's row.
    expect(find.text('ok'), findsOneWidget);
  });

  testWidgets('deleting a remote endpoint removes it from the registry', (
    tester,
  ) async {
    final client = _client();
    await client.registerEndpoint(
      name: 'crm_rest',
      kind: 'openapi',
      baseUrl: 'http://127.0.0.1:9999',
    );
    await _pump(tester, client);
    expect(find.bySemanticsLabel('endpoint-item:crm_rest'), findsOneWidget);

    await tester.tap(find.bySemanticsLabel('endpoint-delete:crm_rest'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('endpoint-item:crm_rest'), findsNothing);
    expect(find.textContaining('no endpoints registered'), findsOneWidget);
    // The registry itself dropped it (not just the row).
    expect(await client.listEndpoints(), isEmpty);
  });

  testWidgets('listing packs shows the empty-state for an overlay-only node', (
    tester,
  ) async {
    // The fixture client subscribes to nothing — the honest default:
    // an isolated spoke that runs on its own overlay.
    await _pump(tester, _client());
    await tester.tap(find.bySemanticsLabel('list-packs-button'));
    await tester.pumpAndSettle();
    expect(find.textContaining('no packs subscribed'), findsOneWidget);
  });

  testWidgets(
    'unsubscribing a pack requires a confirm step and removes the row',
    (tester) async {
      final client = _subscribedClient();
      await _pump(tester, client);
      await tester.tap(find.bySemanticsLabel('list-packs-button'));
      await tester.pumpAndSettle();
      expect(find.bySemanticsLabel('pack-item:crm-essentials'), findsOneWidget);

      // First tap only ARMS the action — nothing is removed yet.
      await tester.tap(
        find.bySemanticsLabel('pack-unsubscribe:crm-essentials'),
      );
      await tester.pumpAndSettle();
      expect(find.bySemanticsLabel('pack-item:crm-essentials'), findsOneWidget);
      expect(
        find.bySemanticsLabel('pack-unsubscribe-confirm:crm-essentials'),
        findsOneWidget,
      );

      // The confirm tap drives the real client and refreshes the list.
      await tester.tap(
        find.bySemanticsLabel('pack-unsubscribe-confirm:crm-essentials'),
      );
      await tester.pumpAndSettle();
      expect(find.bySemanticsLabel('pack-item:crm-essentials'), findsNothing);
      expect(find.textContaining('no packs subscribed'), findsOneWidget);
      // The client really dropped the subscription (not just the row).
      expect(await client.listPacks(), isEmpty);
    },
  );

  testWidgets('the armed confirm stays visible and disabled while the '
      'unsubscribe is in flight', (tester) async {
    final client = _GatedUnsubscribeClient(_subscribedClient());
    await _pump(tester, client);
    await tester.tap(find.bySemanticsLabel('list-packs-button'));
    await tester.pumpAndSettle();
    await tester.tap(find.bySemanticsLabel('pack-unsubscribe:crm-essentials'));
    await tester.pumpAndSettle();

    // Fire the confirm; the client's future is gated open.
    await tester.tap(
      find.bySemanticsLabel('pack-unsubscribe-confirm:crm-essentials'),
    );
    await tester.pump();

    // MID-FLIGHT: the row must NOT flip back to the unarmed icon —
    // the confirm stays visible but disabled (no double-fire).
    final confirm = find.bySemanticsLabel(
      'pack-unsubscribe-confirm:crm-essentials',
    );
    expect(confirm, findsOneWidget);
    final button = tester.widget<FilledButton>(
      find.descendant(of: confirm, matching: find.byType(FilledButton)),
    );
    expect(button.onPressed, isNull);

    // Release the gate — the call completes and the row disappears.
    client.gate.complete();
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('pack-item:crm-essentials'), findsNothing);
    expect(find.textContaining('no packs subscribed'), findsOneWidget);
  });

  testWidgets('the import-pack card renders its paste-based surface', (
    tester,
  ) async {
    await _pump(tester, _client());
    expect(find.bySemanticsLabel('pack-import-manifest-field'), findsOneWidget);
    expect(find.bySemanticsLabel('pack-import-tarball-field'), findsOneWidget);
    expect(find.bySemanticsLabel('pack-import-allow-mismatch'), findsOneWidget);
    expect(find.bySemanticsLabel('pack-import-submit'), findsOneWidget);
  });

  testWidgets('a failing import surfaces the client error verbatim', (
    tester,
  ) async {
    // The fixture client does not implement import_pack — the error
    // message must reach the card unaltered (server codes like
    // `pack_signature_invalid` surface the same way over HTTP). The
    // manifest is complete so the client-side pre-flight lets it pass.
    await _pump(tester, _client());
    await tester.enterText(
      find.bySemanticsLabel('pack-import-manifest-field'),
      '{"id":"crm-essentials","version":1,"content_hash":"sha256:x",'
      '"signature":"hmac:y"}',
    );
    await tester.enterText(
      find.bySemanticsLabel('pack-import-tarball-field'),
      'AAAA',
    );
    await tester.tap(find.bySemanticsLabel('pack-import-submit'));
    await tester.pumpAndSettle();
    expect(find.textContaining('import_pack'), findsOneWidget);
  });

  testWidgets(
    'the import pre-flight refuses a manifest missing required keys',
    (tester) async {
      await _pump(tester, _client());
      await tester.enterText(
        find.bySemanticsLabel('pack-import-manifest-field'),
        '{"id":"crm-essentials","version":1}',
      );
      await tester.enterText(
        find.bySemanticsLabel('pack-import-tarball-field'),
        'AAAA',
      );
      await tester.tap(find.bySemanticsLabel('pack-import-submit'));
      await tester.pumpAndSettle();
      // A precise, named refusal — before any wire round-trip.
      expect(
        find.textContaining('manifest_missing_keys: content_hash, signature'),
        findsOneWidget,
      );
    },
  );

  test('the import pre-flight caps the pasted tarball size', () {
    const manifest =
        '{"id":"p","version":1,"content_hash":"sha256:x","signature":"s"}';
    // Just under the cap: fine.
    expect(
      validatePackImportInput(manifest, 'A' * packImportMaxTarballChars),
      isNull,
    );
    // One over: a precise refusal, no wire round-trip.
    final err = validatePackImportInput(
      manifest,
      'A' * (packImportMaxTarballChars + 1),
    );
    expect(err, contains('tarball_too_large'));
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
