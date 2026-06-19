// Widget test for the outbound webhook delivery log pane. Backed by the
// in-memory fixture client (configured:true) so it runs under
// `flutter test`. The fixture records a successful delivery on every
// captureEvent, so after capturing one event a delivered row shows.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/fixture_escurel_client.dart';
import 'package:escurel_explorer_kit/editor/webhook_deliveries_pane.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

EscurelClient _corpus() => FixtureEscurelClient.fromSources(
  skillFiles: const {},
  instanceFiles: const {},
);

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: WebhookDeliveriesPane())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders the webhook deliveries pane', (tester) async {
    await _pump(tester, _corpus());
    expect(find.bySemanticsLabel('webhook-deliveries-pane'), findsOneWidget);
    expect(find.bySemanticsLabel('webhook-deliveries-refresh'), findsOneWidget);
    // No deliveries captured yet.
    expect(find.text('No deliveries yet'), findsOneWidget);
  });

  testWidgets('a captured event surfaces as a delivered row', (tester) async {
    final client = _corpus();
    // Capture an event — the fixture records a successful delivery.
    await client.captureEvent(
      source: 'manual',
      mime: 'text/plain',
      labelSkill: 'note',
      title: 'hello',
      body: 'hello',
    );

    await _pump(tester, client);

    // The deliveries list renders with the recorded delivery row.
    expect(
      find.byKey(const ValueKey('webhook_deliveries.list')),
      findsOneWidget,
    );
    expect(find.bySemanticsLabel('webhook-delivery-item'), findsOneWidget);
    // The successful HTTP status shows.
    expect(find.text('200'), findsOneWidget);
  });
}
