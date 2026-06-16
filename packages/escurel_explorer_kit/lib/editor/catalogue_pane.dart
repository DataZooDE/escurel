import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../md/frontmatter.dart' as md;
import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/kind_chip.dart';
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
        loading: () => const Center(child: CircularProgressIndicator(strokeWidth: 2)),
        error: (e, _) => _ErrorBlock(message: '$e'),
        data: (skills) => ListView(
          padding: const EdgeInsets.all(8),
          children: [
            for (final s in skills) _SkillTile(skill: s),
          ],
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

    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Theme(
        data: Theme.of(context).copyWith(dividerColor: Colors.transparent),
        child: ExpansionTile(
          initiallyExpanded: true,
          tilePadding: const EdgeInsets.symmetric(horizontal: 8),
          childrenPadding: const EdgeInsets.only(left: 12, right: 4, bottom: 4),
          collapsedShape: const Border(),
          shape: const Border(),
          title: Row(
            children: [
              Expanded(
                child: InkWell(
                  borderRadius: BorderRadius.circular(4),
                  onTap: () => ref.read(currentPageIdProvider.notifier).state = skill.id,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 6),
                    child: Row(
                      children: [
                        const KindChip(pageType: md.PageType.skill),
                        const SizedBox(width: 8),
                        Expanded(child: Text(skill.id, style: text.titleSmall)),
                      ],
                    ),
                  ),
                ),
              ),
            ],
          ),
          children: [
            instancesAsync.when(
              loading: () => const Padding(
                padding: EdgeInsets.symmetric(vertical: 4),
                child: SizedBox(height: 12, width: 12, child: CircularProgressIndicator(strokeWidth: 1.5)),
              ),
              error: (e, _) => _ErrorBlock(message: '$e'),
              data: (instances) => Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  for (final i in instances)
                    _InstanceTile(
                      instance: i,
                      selected: i.id == selectedId,
                      onTap: () => ref.read(currentPageIdProvider.notifier).state = i.id,
                    ),
                  if (instances.isEmpty)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 4, horizontal: 8),
                      child: Text(
                        'no instances yet',
                        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                      ),
                    ),
                  if (editable)
                    _CreateInstanceRow(
                      skill: skill,
                      onTap: () => _openCreate(context, ref, skill),
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _openCreate(BuildContext context, WidgetRef ref, SkillSummary skill) async {
    // Seed a blank draft: every required/optional field empty, body a
    // bare `# <id>` heading the operator fills in.
    final fm = <String, dynamic>{
      'type': 'instance',
      'skill': skill.id,
      'id': '',
      for (final k in skill.requiredFrontmatter) k: '',
    };
    ref.read(pageDraftProvider.notifier).state =
        PageDraft(frontmatter: fm, body: '# Neue Instanz\n');
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
                    Text('Neue Instanz · ${skill.id}',
                        style: Theme.of(ctx).textTheme.titleMedium),
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
              Text('Neue Instanz',
                  style: text.bodySmall?.copyWith(color: kPrimary, fontWeight: FontWeight.w600)),
            ],
          ),
        ),
      ),
    );
  }
}

class _InstanceTile extends StatelessWidget {
  const _InstanceTile({required this.instance, required this.selected, required this.onTap});

  final InstanceSummary instance;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final shortId = instance.id.split('__').last;
    return InkWell(
      borderRadius: BorderRadius.circular(4),
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(
          color: selected ? kSurfaceContainerHigh : Colors.transparent,
          borderRadius: BorderRadius.circular(4),
        ),
        child: Text(shortId, style: text.bodySmall),
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
