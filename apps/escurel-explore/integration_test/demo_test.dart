// Real-browser behavioral verification of the capability demo.
//
// Runs under `flutter drive -d web-server` (Chrome via chromedriver),
// so `tester.tap` / `tester.enterText` go through Flutter's own
// gesture system — unlike DOM events from an external automation
// tool, these actually fire the widget callbacks. Each test drives a
// panel against a REAL escurel-server (pointed at by
// ESCUREL_EXPLORE_BASE_URL) and then re-reads the backend through the
// client to prove the UI action reached the server.
//
// Start order is owned by scripts/drive-demo.sh:
//   server (dev mode, serving nothing special) → chromedriver →
//   flutter drive --dart-define=ESCUREL_EXPLORE_BASE_URL=...
import 'package:escurel_explorer_kit/client/http_escurel_client.dart';
import 'package:escurel_explorer_kit/demo/demo_screen.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _baseUrl = String.fromEnvironment(
  'ESCUREL_EXPLORE_BASE_URL',
  defaultValue: 'http://127.0.0.1:8080',
);

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  Future<HttpEscurelClient> pumpDemo(WidgetTester tester) async {
    final client = HttpEscurelClient(baseUrl: _baseUrl);
    await tester.pumpWidget(
      ProviderScope(
        overrides: [escurelClientProvider.overrideWithValue(client)],
        child: const MaterialApp(home: DemoScreen()),
      ),
    );
    await tester.pumpAndSettle();
    return client;
  }

  testWidgets('Author: Save writes a page reachable over the API', (tester) async {
    final client = await pumpDemo(tester);

    await tester.tap(find.text('Author'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Save (update_page)'));
    await tester.pumpAndSettle(const Duration(seconds: 2));

    // The default Author content authors note::demo — prove it landed.
    final resolved = await client.resolve('[[note::demo]]');
    expect(resolved.exists, isTrue, reason: 'update_page from the UI must create note::demo');
  });

  testWidgets('Chat: Append then the message is readable over the API', (tester) async {
    final client = await pumpDemo(tester);

    await tester.tap(find.text('Chat'));
    await tester.pumpAndSettle();

    final group = 'itest-${DateTime.now().millisecondsSinceEpoch}';
    Finder field(String l) =>
        find.descendant(of: find.bySemanticsLabel(l), matching: find.byType(TextField));
    await tester.enterText(field('chat-group'), group);
    await tester.enterText(field('chat-content'), 'hello from integration_test');
    await tester.tap(find.text('Append'));
    await tester.pumpAndSettle(const Duration(seconds: 2));

    final page = await client.listMessages(group, direction: 'asc');
    expect(
      page.messages.map((m) => m.content),
      contains('hello from integration_test'),
      reason: 'append_message from the UI must reach the backend',
    );

    // And the UI list rendered it.
    expect(find.text('hello from integration_test'), findsWidgets);
  });

  testWidgets('Ops: Load ops fetches admin quota + audit', (tester) async {
    await pumpDemo(tester);

    await tester.tap(find.text('Ops'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Load ops (admin)'));
    await tester.pumpAndSettle(const Duration(seconds: 2));

    // The quota line renders once the admin_quota tool returns.
    expect(find.textContaining('quota —'), findsOneWidget);
    expect(find.textContaining('audit —'), findsOneWidget);
  });

  testWidgets('Search: submitting a query returns without error', (tester) async {
    final client = await pumpDemo(tester);
    // Seed one page so FTS has a lexical hit (ZeroEmbedder makes the
    // vector half inert; BM25 is real).
    await client.updatePage(
      'markdown/instances/note/searchable.md',
      '---\ntype: instance\nskill: note\nid: searchable\ntitle: Searchable\n---\n\n# Searchable\n\nzebra mango lookup token.\n',
    );

    await tester.enterText(
      find.descendant(of: find.bySemanticsLabel('search-input'), matching: find.byType(TextField)),
      'zebra',
    );
    // 'Search' also labels the tab; target the submit button.
    await tester.tap(find.widgetWithText(ElevatedButton, 'Search'));
    await tester.pumpAndSettle(const Duration(seconds: 2));

    // Status line moved off 'idle' (either hits or a clean 0).
    expect(find.textContaining('hits'), findsOneWidget);
  });
}
