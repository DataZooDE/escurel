// The left event pane auto-minimizes while a skill is focused (skills
// carry no events), without clobbering the user's manual collapse toggle.

import 'package:escurel_explore/client/models.dart';
import 'package:escurel_explore/crm/crm_providers.dart';
import 'package:escurel_explore/md/frontmatter.dart';
import 'package:escurel_explore/state/providers.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ExpandResult _page(PageType type) => ExpandResult(
      pageId: 'p',
      skill: 'customer',
      pageType: type,
      frontmatter: const {},
      body: '',
      blocks: const [],
      wikilinksOut: const [],
    );

void main() {
  test('effective collapse is forced on a skill, respects the toggle on an instance', () async {
    // Skill focused → left auto-minimized even though the user toggle is off.
    final skillC = ProviderContainer(
      overrides: [currentPageProvider.overrideWith((ref) async => _page(PageType.skill))],
    );
    addTearDown(skillC.dispose);
    await skillC.read(currentPageProvider.future);
    expect(skillC.read(currentPageIsSkillProvider), isTrue);
    expect(skillC.read(leftCollapsedProvider), isFalse, reason: 'manual toggle untouched');
    expect(skillC.read(effectiveLeftCollapsedProvider), isTrue, reason: 'skill forces minimize');

    // Instance focused → effective follows the manual toggle.
    final instC = ProviderContainer(
      overrides: [currentPageProvider.overrideWith((ref) async => _page(PageType.instance))],
    );
    addTearDown(instC.dispose);
    await instC.read(currentPageProvider.future);
    expect(instC.read(currentPageIsSkillProvider), isFalse);
    expect(instC.read(effectiveLeftCollapsedProvider), isFalse, reason: 'instance, toggle off');
    instC.read(leftCollapsedProvider.notifier).state = true;
    expect(instC.read(effectiveLeftCollapsedProvider), isTrue, reason: 'instance, toggle on');
  });
}
