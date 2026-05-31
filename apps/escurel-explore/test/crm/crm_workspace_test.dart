// Widget test for the M7 CRM workspace shell: breadcrumb + search-top +
// a resizable/collapsible two-pane split (event view left, instance view
// right) + time scrubber + capture-bottom, backed by a small fixture
// corpus so it renders end-to-end under `flutter test`.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/fixture_escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/crm_providers.dart';
import 'package:escurel_explore/crm/crm_workspace.dart';
import 'package:escurel_explore/md/frontmatter.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ExpandResult _skillPage() => const ExpandResult(
      pageId: 'markdown/skills/customer.md',
      skill: 'customer',
      pageType: PageType.skill,
      frontmatter: {'type': 'skill', 'id': 'customer'},
      body: '# customer\n',
      blocks: [],
      wikilinksOut: [],
    );

EscurelClient _corpus() => FixtureEscurelClient.fromSources(
      skillFiles: const {
        'customer.md': '---\ntype: skill\nid: customer\ndescription: A buying org.\n---\n# customer\n',
      },
      instanceFiles: const {
        'customer__acme.md':
            '---\ntype: instance\nskill: customer\nid: acme\nname: Acme Ltd\n---\n# Acme Ltd\n',
      },
    );

Future<void> _pump(WidgetTester tester, {List<Override> overrides = const []}) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [escurelClientProvider.overrideWithValue(_corpus()), ...overrides],
      child: const MaterialApp(home: CrmWorkspace()),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('renders the data-zoo / CRM breadcrumb', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('brand'), findsOneWidget);
    expect(find.bySemanticsLabel('instances'), findsOneWidget);
  });

  testWidgets('search pinned top, capture pinned bottom', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('search-input'), findsOneWidget);
    expect(find.bySemanticsLabel('search-send'), findsOneWidget);
    expect(find.bySemanticsLabel('capture-input'), findsOneWidget);
    expect(find.bySemanticsLabel('capture-send'), findsOneWidget);
  });

  testWidgets('renders both views of one memory (event left, instance right)', (tester) async {
    await _pump(tester);
    expect(find.bySemanticsLabel('region-events'), findsOneWidget);
    expect(find.bySemanticsLabel('region-instance'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget);
    expect(find.bySemanticsLabel('instance-pane'), findsOneWidget);
    // The left view carries the event history + the inbox below it.
    expect(find.bySemanticsLabel('event-history'), findsOneWidget);
    expect(find.bySemanticsLabel('inbox'), findsOneWidget);
    // A draggable divider sits between the two views.
    expect(find.bySemanticsLabel('pane-resize'), findsOneWidget);
  });

  testWidgets('collapsing the left view hides it and offers an expand toggle', (tester) async {
    await _pump(tester, overrides: [leftCollapsedProvider.overrideWith((ref) => true)]);
    // Collapsed: the event pane is gone, an expand toggle remains.
    expect(find.bySemanticsLabel('event-pane'), findsNothing);
    expect(find.bySemanticsLabel('region-events-expand'), findsOneWidget);
    // The instance view still renders.
    expect(find.bySemanticsLabel('instance-pane'), findsOneWidget);
  });

  testWidgets('the event-pane chevron re-opens an auto-minimized skill page', (tester) async {
    // A skill page auto-minimizes the (event-less) left pane. Regression:
    // the chevron used to be dead there — `effective = collapsed || isSkill`
    // meant tapping expand could never beat the skill flag, so the pane
    // could not be opened again. Now the explicit choice wins.
    await _pump(tester, overrides: [
      currentPageProvider.overrideWith((ref) async => _skillPage()),
    ]);

    // Auto-minimized: only the expand toggle shows.
    expect(find.bySemanticsLabel('region-events-expand'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsNothing);

    // Tapping expand must re-open it (collapse toggle + event pane back).
    await tester.tap(find.bySemanticsLabel('region-events-expand'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('region-events-collapse'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget);
  });

  testWidgets('navigate to a skill, then the chevron re-opens the event pane (real nav)', (tester) async {
    // Closer to the live flow than the static override above: a real
    // fixture and an actual navigation to a skill, so `currentPage`
    // resolves async and `isSkill` flips through the real provider
    // machinery (incl. the reset listener) before the chevron is tapped.
    final client = _corpus();
    final container = ProviderContainer(overrides: [escurelClientProvider.overrideWithValue(client)]);
    addTearDown(container.dispose);

    tester.view.physicalSize = const Size(1400, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(home: CrmWorkspace()),
      ),
    );
    await tester.pumpAndSettle();

    // Navigate to the skill page (mirrors focusSkill setting currentPageId).
    final resolved = await client.resolve('[[customer]]');
    expect(resolved.exists, isTrue);
    container.read(currentPageIdProvider.notifier).state = resolved.pageId;
    await tester.pumpAndSettle();

    // Auto-minimized on the skill, then the chevron re-opens it.
    expect(find.bySemanticsLabel('region-events-expand'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsNothing);
    await tester.tap(find.bySemanticsLabel('region-events-expand'));
    await tester.pumpAndSettle();
    expect(find.bySemanticsLabel('region-events-collapse'), findsOneWidget);
    expect(find.bySemanticsLabel('event-pane'), findsOneWidget);
  });
}
