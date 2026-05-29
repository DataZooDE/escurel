// Widget test for the radial skill-wheel: with an entity focused and
// the client returning typed neighbours, the wheel renders one node
// per unique (linkSkill, dst) and a lineage tile for each.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/md/frontmatter.dart';
import 'package:escurel_explore/crm/lineage_rail.dart';
import 'package:escurel_explore/crm/skill_wheel.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _StubClient implements EscurelClient {
  @override
  Future<List<Neighbour>> neighbours(String pageId,
          {LinkDirection direction = LinkDirection.both, String? linkSkill, String? asOf}) async =>
      const [
        Neighbour(src: 'p', dst: 'hoffmann-followup', linkSkill: 'lead'),
        Neighbour(src: 'p', dst: 'hoffmann-pilot', linkSkill: 'opportunity'),
        Neighbour(src: 'p', dst: 'hoffmann-pilot', linkSkill: 'project'),
      ];

  @override
  Future<ExpandResult> expand(String pageId, {String? anchor, String? version, String? asOf}) async =>
      const ExpandResult(
        pageId: 'markdown/instances/engagement__hoffmann-spine.md',
        skill: 'engagement',
        pageType: PageType.instance,
        frontmatter: {},
        body: '',
        blocks: [],
        wikilinksOut: [],
      );

  @override
  Future<ResolveResult> resolve(String wikilink) async => const ResolveResult(
        pageId: 'markdown/x.md',
        skill: 'lead',
        pageType: PageType.instance,
        exists: true,
      );

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

Future<void> _pump(WidgetTester tester, Widget child) async {
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        escurelClientProvider.overrideWithValue(_StubClient()),
        currentPageIdProvider.overrideWith((ref) => 'markdown/instances/engagement__hoffmann-spine.md'),
      ],
      child: MaterialApp(home: Scaffold(body: child)),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('skill-wheel renders one node per unique typed neighbour', (tester) async {
    await _pump(tester, const SkillWheel());
    expect(find.bySemanticsLabel('skill-wheel'), findsOneWidget);
    // 3 unique (linkSkill, dst): lead/…, opportunity/…, project/…
    expect(find.bySemanticsLabel('wheel-node'), findsNWidgets(3));
    expect(find.textContaining('3 links'), findsOneWidget);
  });

  testWidgets('lineage rail lists each typed link', (tester) async {
    await _pump(tester, const LineageRail());
    expect(find.bySemanticsLabel('lineage-rail'), findsOneWidget);
    expect(find.bySemanticsLabel('lineage-link'), findsNWidgets(3));
  });
}
