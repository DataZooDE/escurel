// A base-layer page (`layer: base@<pack>@<version>`, imported from a
// subscribed skill pack) is read-only: the server rejects `update_page`
// with `layer_read_only`, so the explorer's edit affordance
// (currentPageEditableProvider) must stay off for the PAGE itself —
// skill page and imported base instance alike — even when write mode is
// on and the skill is otherwise allowlisted for editing.
//
// Crucially, the skill's OTHER instances stay authorable: a tenant
// specialises a base skill by writing overlay instances, so the layer
// gates the page, never the whole skill (agy review MUST-FIX 2/3).

import 'package:escurel_explorer_kit/client/models.dart';
import 'package:escurel_explorer_kit/config/feature_flags.dart';
import 'package:escurel_explorer_kit/md/frontmatter.dart';
import 'package:escurel_explorer_kit/state/providers.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ExpandResult _page({
  required String pageId,
  required String skill,
  required PageType pageType,
  Map<String, dynamic> frontmatter = const {},
}) => ExpandResult(
  pageId: pageId,
  skill: skill,
  pageType: pageType,
  frontmatter: frontmatter,
  body: '',
  blocks: const [],
  wikilinksOut: const [],
);

SkillSummary _skill(String id, {String layer = 'overlay'}) => SkillSummary(
  id: id,
  description: '',
  requiredFrontmatter: const [],
  optionalFrontmatter: const [],
  layer: layer,
);

ProviderContainer _container(ExpandResult page, {String layer = 'overlay'}) {
  final container = ProviderContainer(
    overrides: [
      writeEnabledProvider.overrideWithValue(true),
      // The skill page is allowlisted as editable — only the base layer
      // may flip it off.
      editableSkillPagesProvider.overrideWithValue({page.skill}),
      skillsCatalogueProvider.overrideWith(
        (ref) async => [_skill(page.skill, layer: layer)],
      ),
      currentPageProvider.overrideWith((ref) async => page),
    ],
  );
  addTearDown(container.dispose);
  return container;
}

void main() {
  test('a base-layer skill page is not editable', () async {
    final container = _container(
      _page(
        pageId: 'markdown/skills/pallet-consolidation.md',
        skill: 'pallet-consolidation',
        pageType: PageType.skill,
        frontmatter: const {'layer': 'base@logistics-midmarket@v7'},
      ),
      layer: 'base@logistics-midmarket@v7',
    );
    await container.read(currentPageProvider.future);
    await container.read(skillsCatalogueProvider.future);
    expect(container.read(currentPageEditableProvider), isFalse);
  });

  test('an imported base-layer instance is not editable', () async {
    final container = _container(
      _page(
        pageId: 'markdown/instances/pallet-consolidation/edge.md',
        skill: 'pallet-consolidation',
        pageType: PageType.instance,
        frontmatter: const {'layer': 'base@logistics-midmarket@v7'},
      ),
      layer: 'base@logistics-midmarket@v7',
    );
    await container.read(currentPageProvider.future);
    await container.read(skillsCatalogueProvider.future);
    expect(container.read(currentPageEditableProvider), isFalse);
  });

  test('a tenant overlay instance of a base skill stays authorable', () async {
    // The specialisation story: base skill, tenant-authored instance —
    // no `layer` in the instance frontmatter ⇒ editable per normal rules.
    final container = _container(
      _page(
        pageId: 'markdown/instances/pallet-consolidation/my-note.md',
        skill: 'pallet-consolidation',
        pageType: PageType.instance,
      ),
      layer: 'base@logistics-midmarket@v7',
    );
    await container.read(currentPageProvider.future);
    await container.read(skillsCatalogueProvider.future);
    expect(
      container.read(currentPageEditableProvider),
      isTrue,
      reason: 'instances of a base skill are the tenant overlay — editable',
    );
    // And the instance-level edit gate agrees: the base layer never
    // blocks the whole skill.
    expect(
      container.read(skillEditableProvider)('pallet-consolidation'),
      isTrue,
    );
  });
}
