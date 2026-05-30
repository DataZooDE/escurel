// Widget test for the ☰ skills registry: opening it lists skills grouped
// ENTITY-BOUND / EVENT-TYPED; clicking a skill opens its manifest.

import 'package:escurel_explore/client/escurel_client.dart';
import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/crm_breadcrumb.dart';
import 'package:escurel_explore/md/frontmatter.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _customerSkill = 'markdown/skills/customer.md';

class _StubClient implements EscurelClient {
  @override
  Future<List<SkillSummary>> listSkills() async => const [
        SkillSummary(
          id: 'customer',
          description: 'A buying organisation.',
          requiredFrontmatter: [],
          optionalFrontmatter: [],
          isEventTyped: false,
        ),
        SkillSummary(
          id: 'meeting',
          description: 'A meeting/call artifact.',
          requiredFrontmatter: [],
          optionalFrontmatter: [],
          isEventTyped: true,
        ),
      ];

  @override
  Future<List<InstanceSummary>> listInstances(String skillId,
          {Map<String, Object?>? filter, String? orderBy, int? limit, String? asOf, String? scenario}) async =>
      const [];

  @override
  Future<ResolveResult> resolve(String wikilink, {String? scenario}) async {
    if (wikilink == '[[customer]]') {
      return const ResolveResult(
        pageId: _customerSkill,
        skill: 'customer',
        pageType: PageType.skill,
        exists: true,
      );
    }
    return const ResolveResult(pageId: '', skill: '', pageType: PageType.instance, exists: false);
  }

  @override
  dynamic noSuchMethod(Invocation i) => throw UnimplementedError('${i.memberName}');
}

void main() {
  testWidgets('the ☰ menu lists grouped skills and opens one on tap', (tester) async {
    tester.view.physicalSize = const Size(1200, 800);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final container = ProviderContainer(
      overrides: [escurelClientProvider.overrideWithValue(_StubClient())],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(appBar: CrmBreadcrumb(), body: SizedBox.shrink()),
        ),
      ),
    );
    await tester.pumpAndSettle();

    // Closed initially.
    expect(find.bySemanticsLabel('skills-menu'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:customer'), findsNothing);

    // Open it → grouped rows.
    await tester.tap(find.bySemanticsLabel('skills-menu'));
    await tester.pumpAndSettle();
    expect(find.text('ENTITY-BOUND · 1'), findsOneWidget);
    expect(find.text('EVENT-TYPED · 1'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:customer'), findsOneWidget);
    expect(find.bySemanticsLabel('skill-row:meeting'), findsOneWidget);

    // Click the customer skill → opens its manifest, menu closes.
    await tester.tap(find.bySemanticsLabel('skill-row:customer'));
    await tester.pumpAndSettle();
    expect(container.read(currentPageIdProvider), _customerSkill);
    expect(find.bySemanticsLabel('skill-row:customer'), findsNothing);
  });
}
