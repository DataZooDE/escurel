import 'package:flutter/material.dart';

import '../theme/app_theme.dart';
import 'skill_avatar.dart';

/// A clickable pill carrying a skill-coloured [SkillAvatar] + a label —
/// the recurring "go to this entity" affordance in the links footer and
/// the breadcrumb menus.
class EntityChip extends StatelessWidget {
  const EntityChip({
    super.key,
    required this.skill,
    required this.label,
    required this.onTap,
    this.semanticsLabel,
  });

  final String skill;
  final String label;
  final VoidCallback onTap;
  final String? semanticsLabel;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    final body = InkWell(
      borderRadius: BorderRadius.circular(999),
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.fromLTRB(3, 3, 10, 3),
        decoration: BoxDecoration(
          color: kSurfaceContainerLow,
          borderRadius: BorderRadius.circular(999),
          border: Border.all(color: kOutlineVariant),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            SkillAvatar(skill: skill, size: 20),
            const SizedBox(width: 6),
            Text(
              label,
              style: text.labelMedium?.copyWith(color: kOnSurface, fontWeight: FontWeight.w600),
            ),
          ],
        ),
      ),
    );
    if (semanticsLabel == null) return body;
    return Semantics(
      label: semanticsLabel,
      button: true,
      onTap: onTap,
      excludeSemantics: true,
      child: body,
    );
  }
}
