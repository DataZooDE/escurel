/// The ☰ skills registry — a leading top-bar dropdown listing every
/// skill ("manifests that define what instances carry"), grouped
/// ENTITY-BOUND vs EVENT-TYPED. Clicking a skill opens its manifest
/// (`focusSkill`); since skills carry no events, the workspace minimizes
/// the left event pane while a skill is focused (see
/// `effectiveLeftCollapsedProvider`).
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';
import '../widgets/breadcrumb_menu.dart';
import '../widgets/skill_avatar.dart';

class SkillsMenu extends StatelessWidget {
  const SkillsMenu({super.key});

  @override
  Widget build(BuildContext context) {
    return BreadcrumbMenu(
      trigger: (open) => Semantics(
        label: 'skills-menu',
        button: true,
        onTap: open,
        excludeSemantics: true,
        child: IconButton(
          icon: const Icon(Icons.menu, color: kOnSurface),
          tooltip: 'Skill templates',
          onPressed: open,
        ),
      ),
      panelBuilder: (close) => _SkillsPanel(close: close),
    );
  }
}

class _SkillsPanel extends ConsumerWidget {
  const _SkillsPanel({required this.close});
  final VoidCallback close;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skills = ref.watch(skillsCatalogueProvider);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const MenuHeader(
          title: 'Skill templates · registry',
          subtitle: 'manifests that define what instances carry',
        ),
        Flexible(
          child: skills.when(
            loading: () => const _PanelLoading(),
            error: (e, _) => _PanelError('$e'),
            data: (list) {
              int byId(SkillSummary a, SkillSummary b) => a.id.compareTo(b.id);
              final entity = list.where((s) => !s.isEventTyped).toList()..sort(byId);
              final event = list.where((s) => s.isEventTyped).toList()..sort(byId);
              return ListView(
                padding: EdgeInsets.zero,
                shrinkWrap: true,
                children: [
                  if (entity.isNotEmpty) MenuSectionHeader(label: 'ENTITY-BOUND', count: entity.length),
                  for (final s in entity) _SkillRow(skill: s, onTap: () => _open(ref, s.id)),
                  if (event.isNotEmpty) MenuSectionHeader(label: 'EVENT-TYPED', count: event.length),
                  for (final s in event) _SkillRow(skill: s, onTap: () => _open(ref, s.id)),
                ],
              );
            },
          ),
        ),
        const MenuFooter(hint: 'click to open the skill manifest'),
      ],
    );
  }

  void _open(WidgetRef ref, String skillId) {
    close();
    focusSkill(ref, skillId);
  }
}

class _SkillRow extends StatelessWidget {
  const _SkillRow({required this.skill, required this.onTap});
  final SkillSummary skill;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'skill-row:${skill.id}',
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: InkWell(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 9, 16, 9),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SkillAvatar(skill: skill.id, size: 22),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      skill.id,
                      style: text.bodyMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w700),
                    ),
                    if (skill.description.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      Text(
                        skill.description,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: text.bodySmall?.copyWith(color: kOnSurfaceVariant),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _PanelLoading extends StatelessWidget {
  const _PanelLoading();
  @override
  Widget build(BuildContext context) => const Padding(
        padding: EdgeInsets.all(24),
        child: Center(child: CircularProgressIndicator(strokeWidth: 2)),
      );
}

class _PanelError extends StatelessWidget {
  const _PanelError(this.msg);
  final String msg;
  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.all(16),
        child: Text('error: $msg', style: const TextStyle(color: kError)),
      );
}
