// Widget test for the Dev Inspector's Runtimes panel (#818): every runner
// `list_runtimes` reports, its status in words and by icon shape, what it
// offers (harnesses, labels) and what it is running. Only `list_runtimes`
// is answered here; everything else fails loudly via noSuchMethod.

import 'package:escurel_explorer_kit/client/escurel_client.dart';
import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/inspector/runtimes_panel.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _RuntimesClient implements EscurelClient {
  _RuntimesClient(this.runtimes, {this.error});

  final List<RuntimeInfo> runtimes;
  final Object? error;

  @override
  Future<List<RuntimeInfo>> listRuntimes() async {
    if (error != null) throw error!;
    return runtimes;
  }

  @override
  void close() {}

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<void> _pump(WidgetTester tester, EscurelClient client) async {
  tester.view.physicalSize = const Size(1200, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(client)],
      child: const MaterialApp(home: Scaffold(body: RuntimesPanel())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets(
    'lists each runtime with its status in words, harnesses, labels and live runs',
    (tester) async {
      await _pump(
        tester,
        _RuntimesClient(const [
          RuntimeInfo(
            runnerId: 'build-box',
            pageId: 'markdown/instances/runtime/build-box.md',
            status: 'stale',
            harnesses: [
              RuntimeHarness(name: 'codex', version: 'codex-cli 0.168.0'),
            ],
            ageS: 1200,
            staleAfterS: 90,
          ),
          RuntimeInfo(
            runnerId: 'laptop',
            pageId: 'markdown/instances/runtime/laptop.md',
            status: 'online',
            harnesses: [
              RuntimeHarness(name: 'echo'),
              RuntimeHarness(name: 'claude', version: '2.1.3'),
            ],
            labels: ['repo:escurel', 'site:muc'],
            maxConcurrent: 2,
            ageS: 12,
            staleAfterS: 90,
            liveRuns: 1,
          ),
        ]),
      );

      expect(find.bySemanticsLabel('runtimes-panel'), findsOneWidget);
      expect(find.bySemanticsLabel('runtime-row:laptop'), findsOneWidget);
      expect(find.bySemanticsLabel('runtime-row:build-box'), findsOneWidget);
      expect(find.text('RUNTIMES · 2 · 1 online'), findsOneWidget);
      expect(find.text('online'), findsOneWidget);
      expect(find.text('stale'), findsOneWidget);
      expect(find.text('echo, claude 2.1.3'), findsOneWidget);
      expect(find.text('labels: repo:escurel, site:muc'), findsOneWidget);
      expect(find.text('labels: none'), findsOneWidget);
      expect(find.text('1 live run · at most 2'), findsOneWidget);
      expect(
        find.text('last seen 20 min ago · stale after 90 s'),
        findsOneWidget,
      );
      // Status is never colour alone: each one has an icon of its own shape.
      expect(find.byIcon(Icons.check_circle_outline), findsOneWidget);
      expect(find.byIcon(Icons.warning_amber_outlined), findsOneWidget);
    },
  );

  testWidgets('a harness whose CLI probe failed is unavailable, in words', (
    tester,
  ) async {
    await _pump(
      tester,
      _RuntimesClient(const [
        RuntimeInfo(
          runnerId: 'laptop',
          pageId: 'markdown/instances/runtime/laptop.md',
          status: 'online',
          harnesses: [
            RuntimeHarness(name: 'echo', version: '1.0'),
            RuntimeHarness(
              name: 'codex',
              available: false,
              probeError: '`codex --version` could not start',
            ),
          ],
        ),
      ]),
    );
    expect(find.text('echo 1.0, codex (unavailable)'), findsOneWidget);
    expect(
      find.text(
        'codex unavailable: its CLI probe failed '
        '(`codex --version` could not start)',
      ),
      findsOneWidget,
    );
  });

  testWidgets('a member read (no versions) lists harnesses by name', (
    tester,
  ) async {
    await _pump(
      tester,
      _RuntimesClient(const [
        RuntimeInfo(
          runnerId: 'laptop',
          pageId: 'markdown/instances/runtime/laptop.md',
          status: 'stopped',
          harnesses: [
            RuntimeHarness(name: 'echo'),
            RuntimeHarness(name: 'claude'),
          ],
        ),
      ]),
    );
    expect(find.text('echo, claude'), findsOneWidget);
    expect(find.text('stopped'), findsOneWidget);
    expect(find.byIcon(Icons.stop_circle_outlined), findsOneWidget);
  });

  testWidgets('no runtimes says so, and a refusal is shown in words', (
    tester,
  ) async {
    await _pump(tester, _RuntimesClient(const []));
    expect(
      find.text('no runtimes yet — a runner registers here when it starts'),
      findsOneWidget,
    );
    await _pump(tester, _RuntimesClient(const [], error: StateError('boom')));
    expect(find.textContaining('error:'), findsOneWidget);
  });
}
