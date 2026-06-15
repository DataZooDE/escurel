/// The "Instances N" breadcrumb crumb, as a dropdown: the root directory
/// of every instance in the tenant, grouped by skill. Clicking a row
/// re-centres the workspace on that instance (records the Back trail).
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/breadcrumb_menu.dart';
import '../widgets/skill_avatar.dart';
import 'crm_providers.dart';

class InstancesMenu extends ConsumerWidget {
  const InstancesMenu({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final count = ref
        .watch(allInstancesProvider)
        .maybeWhen(data: (xs) => xs.length, orElse: () => null);
    return BreadcrumbMenu(
      trigger: (open) => Semantics(
        label: 'instances',
        button: true,
        onTap: open,
        excludeSemantics: true,
        child: InkWell(
          onTap: open,
          borderRadius: BorderRadius.circular(6),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  count == null ? 'Instances …' : 'Instances $count',
                  style: text.labelLarge?.copyWith(color: kOnSurfaceVariant),
                ),
                const Icon(Icons.arrow_drop_down, size: 18, color: kOutline),
              ],
            ),
          ),
        ),
      ),
      panelBuilder: (close) => _InstancesPanel(close: close),
    );
  }
}

class _InstancesPanel extends ConsumerWidget {
  const _InstancesPanel({required this.close});
  final VoidCallback close;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final instances = ref.watch(allInstancesProvider);
    final current = ref.watch(currentPageIdProvider);
    final showErased = ref.watch(showErasedProvider);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        MenuHeader(
          title:
              'Instances · ${instances.maybeWhen(data: (xs) => xs.length, orElse: () => '…')}',
          subtitle: 'the root directory of instances',
        ),
        _ShowErasedToggle(
          value: showErased,
          onChanged: (v) => ref.read(showErasedProvider.notifier).state = v,
        ),
        Flexible(
          child: instances.when(
            loading: () => const Padding(
              padding: EdgeInsets.all(24),
              child: Center(child: CircularProgressIndicator(strokeWidth: 2)),
            ),
            error: (e, _) => Padding(
              padding: const EdgeInsets.all(16),
              child: Text('error: $e', style: const TextStyle(color: kError)),
            ),
            data: (list) {
              final groups = <String, List<InstanceSummary>>{};
              for (final i in list) {
                groups.putIfAbsent(i.skill, () => []).add(i);
              }
              final skillIds = groups.keys.toList()..sort();
              return ListView(
                padding: EdgeInsets.zero,
                shrinkWrap: true,
                children: [
                  for (final sk in skillIds) ...[
                    MenuSectionHeader(
                      label: sk.toUpperCase(),
                      count: groups[sk]!.length,
                    ),
                    for (final inst
                        in (groups[sk]!
                          ..sort((a, b) => _name(a).compareTo(_name(b)))))
                      _InstanceRow(
                        instance: inst,
                        selected: inst.id == current,
                        erased: inst.erased,
                        onTap: () {
                          close();
                          navigateToInstance(ref, inst.id);
                        },
                      ),
                  ],
                ],
              );
            },
          ),
        ),
        const MenuFooter(hint: 'click to open the instance'),
      ],
    );
  }
}

/// `markdown/instances/customer__muenchner-pharma.md` → `muenchner-pharma`.
String _slug(String pageId) {
  final base = pageId.split('/').last.replaceAll('.md', '');
  final parts = base.split('__');
  return parts.length == 2 ? parts[1] : base;
}

String _name(InstanceSummary i) =>
    (i.frontmatter['name'] as String?)?.trim().isNotEmpty == true
    ? (i.frontmatter['name'] as String).trim()
    : _slug(i.id);

/// A compact "show deleted" switch at the top of the directory. Defaults
/// off; flipping it reveals tombstones (struck-through rows).
class _ShowErasedToggle extends StatelessWidget {
  const _ShowErasedToggle({required this.value, required this.onChanged});
  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'show-erased-toggle',
      toggled: value,
      button: true,
      onTap: () => onChanged(!value),
      excludeSemantics: true,
      child: InkWell(
        onTap: () => onChanged(!value),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 12, 4),
          child: Row(
            children: [
              Icon(
                value ? Icons.visibility : Icons.visibility_off,
                size: 16,
                color: kOutline,
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  'Gelöschte anzeigen',
                  style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                ),
              ),
              Switch(value: value, onChanged: onChanged),
            ],
          ),
        ),
      ),
    );
  }
}

class _InstanceRow extends StatelessWidget {
  const _InstanceRow({
    required this.instance,
    required this.selected,
    required this.onTap,
    this.erased = false,
  });
  final InstanceSummary instance;
  final bool selected;
  final bool erased;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'instance-row:${_slug(instance.id)}',
      button: true,
      selected: selected,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        child: Container(
          color: selected ? kSurfaceContainerHigh : null,
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
          child: Row(
            children: [
              Opacity(
                opacity: erased ? 0.55 : 1.0,
                child: SkillAvatar(skill: instance.skill, size: 20),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  _name(instance),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: text.bodyMedium?.copyWith(
                    color: erased
                        ? kOutline
                        : (selected ? kPrimary : kOnSurface),
                    fontWeight: selected ? FontWeight.w700 : FontWeight.w500,
                    decoration: erased ? TextDecoration.lineThrough : null,
                  ),
                ),
              ),
              const SizedBox(width: 8),
              if (erased)
                Text(
                  'gelöscht',
                  style: text.labelSmall?.copyWith(color: kError),
                )
              else
                Text(
                  _slug(instance.id),
                  style: text.labelSmall?.copyWith(color: kOutline),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
