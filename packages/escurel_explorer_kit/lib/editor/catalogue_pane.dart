import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../md/frontmatter.dart' as md;
import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/backend_badge.dart';
import '../widgets/kind_chip.dart';
import '../widgets/layer_badge.dart';
import 'page_form.dart';

/// Left pane — skills catalogue with their instances expandable
/// inline. Clicking an instance opens it in the editor.
class CataloguePane extends ConsumerWidget {
  const CataloguePane({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final async = ref.watch(skillsCatalogueProvider);
    final scheme = Theme.of(context).colorScheme;

    return Container(
      key: const ValueKey('pane.catalogue'),
      color: scheme.surfaceContainerLow,
      child: async.when(
        loading: () =>
            const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => _ErrorBlock(message: '$e'),
        data: (skills) => ListView(
          padding: const EdgeInsets.all(8),
          children: [for (final s in skills) _SkillTile(skill: s)],
        ),
      ),
    );
  }
}

class _SkillTile extends ConsumerWidget {
  const _SkillTile({required this.skill});

  final SkillSummary skill;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final instancesAsync = ref.watch(instancesProvider(skill.id));
    final text = Theme.of(context).textTheme;
    final selectedId = ref.watch(currentPageIdProvider);
    final editable = ref.watch(skillEditableProvider)(skill.id);
    // Collapsed-state lives in a provider (not transient ExpansionTile state)
    // so it survives the periodic auto-refresh that rebuilds the catalogue.
    final collapsed = ref.watch(collapsedSkillsProvider).contains(skill.id);

    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Header: the title focuses the skill PAGE; the chevron (its own
          // button, so the tap target is unambiguous) collapses the tile.
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: Row(
              children: [
                Expanded(
                  child: InkWell(
                    borderRadius: BorderRadius.circular(4),
                    // Focus the skill PAGE. `skill.id` is the bare id
                    // (`team_doc`), not a page_id — set it directly and
                    // `expand` finds nothing → empty page. `focusSkill`
                    // resolves `[[id]]` → `markdown/skills/<id>.md` first
                    // (same path the CRM skills-menu uses).
                    onTap: () => focusSkill(ref, skill.id),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 6),
                      child: Row(
                        children: [
                          const KindChip(pageType: md.PageType.skill),
                          const SizedBox(width: 8),
                          Expanded(
                            child: Text(skill.id, style: text.titleSmall),
                          ),
                          if (skill.backendKind != 'markdown') ...[
                            BackendBadge(
                              skillId: skill.id,
                              backendKind: skill.backendKind,
                              writable: skill.capabilities.writable,
                            ),
                            const SizedBox(width: 4),
                          ],
                          if (skill.isBaseLayer) ...[
                            LayerBadge(skillId: skill.id, layer: skill.layer),
                            const SizedBox(width: 4),
                          ],
                          if (skill.shadows != null) ...[
                            ShadowBadge(
                              skillId: skill.id,
                              shadows: skill.shadows!,
                            ),
                            const SizedBox(width: 4),
                          ],
                          if (skill.acl != null) _AclBadge(acl: skill.acl!),
                        ],
                      ),
                    ),
                  ),
                ),
                _CollapseToggle(
                  skillId: skill.id,
                  collapsed: collapsed,
                  onTap: () {
                    final set = {...ref.read(collapsedSkillsProvider)};
                    if (collapsed) {
                      set.remove(skill.id);
                    } else {
                      set.add(skill.id);
                    }
                    ref.read(collapsedSkillsProvider.notifier).state = set;
                  },
                ),
              ],
            ),
          ),
          if (!collapsed)
            Padding(
              padding: const EdgeInsets.only(left: 12, right: 4, bottom: 4),
              child: instancesAsync.when(
                loading: () => const Padding(
                  padding: EdgeInsets.symmetric(vertical: 4),
                  child: SizedBox(
                    height: 12,
                    width: 12,
                    child: CircularProgressIndicator(strokeWidth: 1.5),
                  ),
                ),
                error: (e, _) => _ErrorBlock(message: '$e'),
                data: (instances) {
                // Archived instances (`archived: true`) are hidden per skill by
                // default; the per-skill toggle below reveals them (muted).
                final showArchived = ref
                    .watch(showArchivedSkillsProvider)
                    .contains(skill.id);
                final active = instances.where((i) => !i.archived).toList();
                final archived = instances.where((i) => i.archived).toList();
                final visible = [...active, if (showArchived) ...archived];
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    for (final i in visible)
                      _InstanceTile(
                        instance: i,
                        selected: i.id == selectedId,
                        archived: i.archived,
                        onTap: () => ref
                            .read(currentPageIdProvider.notifier)
                            .state = i.id,
                      ),
                    if (instances.isEmpty)
                      Padding(
                        padding: const EdgeInsets.symmetric(
                          vertical: 4,
                          horizontal: 8,
                        ),
                        child: Text(
                          'no instances yet',
                          style: text.bodySmall?.copyWith(
                            color: kOnSurfaceVariant,
                          ),
                        ),
                      ),
                    if (archived.isNotEmpty)
                      _ShowArchivedToggle(
                        skillId: skill.id,
                        count: archived.length,
                        value: showArchived,
                        onChanged: (v) {
                          final set = {
                            ...ref.read(showArchivedSkillsProvider),
                          };
                          if (v) {
                            set.add(skill.id);
                          } else {
                            set.remove(skill.id);
                          }
                          ref
                                  .read(showArchivedSkillsProvider.notifier)
                                  .state =
                              set;
                        },
                      ),
                    if (editable)
                      _CreateInstanceRow(
                        skill: skill,
                        onTap: () => _openCreate(context, ref, skill),
                      ),
                  ],
                );
                },
              ),
            ),
        ],
      ),
    );
  }

  Future<void> _openCreate(
    BuildContext context,
    WidgetRef ref,
    SkillSummary skill,
  ) async {
    // Seed a blank draft: every required/optional field empty, body a
    // bare `# <id>` heading the operator fills in.
    final fm = <String, dynamic>{
      'type': 'instance',
      'skill': skill.id,
      'id': '',
      for (final k in skill.requiredFrontmatter) k: '',
    };
    ref.read(pageDraftProvider.notifier).state = PageDraft(
      frontmatter: fm,
      body: '# Neue Instanz\n',
    );
    ref.read(pageValidationProvider.notifier).state = const [];
    ref.read(pageSaveProvider.notifier).state = SaveState.idle;

    await showDialog<void>(
      context: context,
      builder: (ctx) => UncontrolledProviderScope(
        // The dialog route mounts under the root Navigator, outside the
        // explorer's isolated ProviderScope — re-attach the same
        // container so the form's providers resolve.
        container: ProviderScope.containerOf(context),
        child: Dialog(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 560, maxHeight: 640),
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: SingleChildScrollView(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      'Neue Instanz · ${skill.id}',
                      style: Theme.of(ctx).textTheme.titleMedium,
                    ),
                    const SizedBox(height: 16),
                    PageEditForm(
                      skill: skill,
                      pageId: '',
                      baseVersion: null,
                      isNew: true,
                      onDone: (focus) {
                        Navigator.of(ctx).pop();
                        if (focus != null) {
                          navigateToInstance(ref, focus);
                        }
                      },
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
    // If the dialog was dismissed without finishing, clear the draft.
    ref.read(pageDraftProvider.notifier).state = null;
  }
}

/// Compact read-only badge of a skill's group ACL (group ACL v1), shown
/// beside the skill id in the catalogue. A shield icon (lock when the
/// instances are owner-scoped) keeps the row narrow; the tooltip carries
/// the full per-CRUD block. `null` group lists render as `default` (the
/// verb falls through to the tenant default); empty lists as `none`
/// (admin-only). The `skill-acl` Semantics label marks its presence.
class _AclBadge extends StatelessWidget {
  const _AclBadge({required this.acl});

  final SkillAcl acl;

  static String _fmt(List<String>? g) =>
      g == null ? 'default' : (g.isEmpty ? 'none' : g.join(', '));

  @override
  Widget build(BuildContext context) {
    final ownerScoped = acl.update?.contains('owner') ?? false;
    return Semantics(
      label: 'skill-acl',
      child: Tooltip(
        message:
            'read: ${_fmt(acl.read)}\n'
            'create: ${_fmt(acl.create)}\n'
            'update: ${_fmt(acl.update)}\n'
            'delete: ${_fmt(acl.delete)}',
        child: Padding(
          padding: const EdgeInsets.only(left: 6),
          child: Icon(
            ownerScoped ? Icons.lock_outline : Icons.shield_outlined,
            size: 14,
            color: kOnSurfaceVariant,
          ),
        ),
      ),
    );
  }
}

class _CreateInstanceRow extends StatelessWidget {
  const _CreateInstanceRow({required this.skill, required this.onTap});

  final SkillSummary skill;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'create-instance:${skill.id}',
      identifier: 'create-instance:${skill.id}',
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        borderRadius: BorderRadius.circular(4),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
          child: Row(
            children: [
              const Icon(Icons.add, size: 14, color: kPrimary),
              const SizedBox(width: 6),
              Text(
                'Neue Instanz',
                style: text.bodySmall?.copyWith(
                  color: kPrimary,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The bare, human-readable slug for an instance's page id. The backend
/// returns a path (`markdown/instances/<skill>/<slug>.md`) and the fixture
/// client a `<skill>__<slug>` handle; both collapse to `<slug>` — showing
/// the full path in the catalogue list is just noise.
String instanceShortLabel(String pageId) {
  var s = pageId
      .split('/')
      .last; // drop any `markdown/instances/<skill>/` prefix
  if (s.endsWith('.md')) s = s.substring(0, s.length - 3);
  return s.split('__').last; // drop the fixture `<skill>__` prefix
}

class _InstanceTile extends StatelessWidget {
  const _InstanceTile({
    required this.instance,
    required this.selected,
    required this.onTap,
    this.archived = false,
  });

  final InstanceSummary instance;
  final bool selected;
  final bool archived;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final shortId = instanceShortLabel(instance.id);
    return Semantics(
      label: 'catalogue-instance:$shortId',
      button: true,
      selected: selected,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        borderRadius: BorderRadius.circular(4),
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          decoration: BoxDecoration(
            color: selected ? kSurfaceContainerHigh : Colors.transparent,
            borderRadius: BorderRadius.circular(4),
          ),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  shortId,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: text.bodySmall?.copyWith(
                    color: archived ? kOnSurfaceVariant : null,
                    fontStyle: archived ? FontStyle.italic : null,
                  ),
                ),
              ),
              if (archived) ...[
                const SizedBox(width: 6),
                const Icon(
                  Icons.inventory_2_outlined,
                  size: 12,
                  color: kOnSurfaceVariant,
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// A per-skill "show archived" control at the foot of a skill's instance list
/// in the catalogue. Defaults off; flipping it reveals the skill's archived
/// instances (rendered muted/italic). Shown only when the skill has any.
class _ShowArchivedToggle extends StatelessWidget {
  const _ShowArchivedToggle({
    required this.skillId,
    required this.count,
    required this.value,
    required this.onChanged,
  });

  final String skillId;
  final int count;
  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'show-archived-toggle:$skillId',
      toggled: value,
      button: true,
      onTap: () => onChanged(!value),
      excludeSemantics: true,
      child: InkWell(
        borderRadius: BorderRadius.circular(4),
        onTap: () => onChanged(!value),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
          child: Row(
            children: [
              Icon(
                value ? Icons.visibility : Icons.visibility_off,
                size: 13,
                color: kOnSurfaceVariant,
              ),
              const SizedBox(width: 6),
              Text(
                value ? '$count archiviert ausblenden' : '$count archiviert',
                style: text.bodySmall?.copyWith(
                  color: kOnSurfaceVariant,
                  fontWeight: FontWeight.w500,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The skill tile's expand/collapse control — a dedicated button so the tap
/// target is unambiguous (the header itself focuses the skill page). State is
/// driven by [collapsedSkillsProvider] so it persists across auto-refresh.
class _CollapseToggle extends StatelessWidget {
  const _CollapseToggle({
    required this.skillId,
    required this.collapsed,
    required this.onTap,
  });

  final String skillId;
  final bool collapsed;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: 'collapse-toggle:$skillId',
      toggled: !collapsed,
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: IconButton(
        icon: Icon(collapsed ? Icons.expand_more : Icons.expand_less),
        iconSize: 20,
        color: kOnSurfaceVariant,
        visualDensity: VisualDensity.compact,
        padding: EdgeInsets.zero,
        constraints: const BoxConstraints(minWidth: 32, minHeight: 32),
        tooltip: collapsed ? 'Ausklappen' : 'Einklappen',
        onPressed: onTap,
      ),
    );
  }
}

class _ErrorBlock extends StatelessWidget {
  const _ErrorBlock({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(8),
      child: Text(
        message,
        style: Theme.of(context).textTheme.bodySmall?.copyWith(color: kError),
      ),
    );
  }
}
