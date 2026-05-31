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
  test('skill auto-minimizes by default, but an explicit choice always wins', () async {
    // Skill focused, no explicit choice → auto-minimized.
    final skillC = ProviderContainer(
      overrides: [currentPageProvider.overrideWith((ref) async => _page(PageType.skill))],
    );
    addTearDown(skillC.dispose);
    await skillC.read(currentPageProvider.future);
    expect(skillC.read(currentPageIsSkillProvider), isTrue);
    expect(skillC.read(leftCollapsedProvider), isNull, reason: 'no explicit choice yet');
    expect(skillC.read(effectiveLeftCollapsedProvider), isTrue, reason: 'skill default = minimized');

    // The chevron writes an explicit choice — which must re-open the pane
    // even on a skill page (regression: the OR-with-skill made it dead).
    skillC.read(leftCollapsedProvider.notifier).state = false;
    expect(skillC.read(effectiveLeftCollapsedProvider), isFalse, reason: 'explicit expand wins on a skill');

    // Instance focused, no explicit choice → events shown.
    final instC = ProviderContainer(
      overrides: [currentPageProvider.overrideWith((ref) async => _page(PageType.instance))],
    );
    addTearDown(instC.dispose);
    await instC.read(currentPageProvider.future);
    expect(instC.read(currentPageIsSkillProvider), isFalse);
    expect(instC.read(effectiveLeftCollapsedProvider), isFalse, reason: 'instance default = shown');
    instC.read(leftCollapsedProvider.notifier).state = true;
    expect(instC.read(effectiveLeftCollapsedProvider), isTrue, reason: 'instance, explicit collapse');
  });
}
