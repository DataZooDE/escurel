import 'package:flutter/material.dart';

import '../theme/app_theme.dart';

/// Stable per-skill spoke colours (cycled by skill name). Shared by the
/// skill-wheel, the links footer, and the breadcrumb menus so a given
/// skill is always the same colour across the workspace.
const _spokePalette = <Color>[
  kPrimary,
  kSecondary,
  kInfo,
  kSuccess,
  kWarning,
  Color(0xFF6A4C93),
  Color(0xFFB5651D),
  Color(0xFF1D5962),
];

/// The stable colour for a skill (cycled by name hash).
Color skillColor(String skill) => _spokePalette[skill.hashCode.abs() % _spokePalette.length];

/// A small skill-coloured circle carrying the skill's initial — the
/// recurring "entity face" in the wheel, link chips, and menu rows.
class SkillAvatar extends StatelessWidget {
  const SkillAvatar({super.key, required this.skill, this.size = 22});

  final String skill;
  final double size;

  @override
  Widget build(BuildContext context) {
    final color = skillColor(skill);
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.16),
        shape: BoxShape.circle,
        border: Border.all(color: color, width: 1.5),
      ),
      child: Text(
        skill.isEmpty ? '?' : skill[0].toUpperCase(),
        style: TextStyle(
          color: color,
          fontWeight: FontWeight.w700,
          fontSize: size * 0.5,
          height: 1,
        ),
      ),
    );
  }
}
