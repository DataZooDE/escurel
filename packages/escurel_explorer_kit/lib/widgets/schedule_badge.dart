import 'package:flutter/material.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';

/// "0 9 * * 1-5 · ≤3/day" — a skill's `schedule:` cron as written, plus its
/// `schedule_budget.runs_per_day` cap when one is set. Null when the skill
/// is not scheduled.
String? scheduleLabel(SkillSummary skill) {
  final cron = skill.schedule;
  if (cron == null || cron.trim().isEmpty) return null;
  final cap = skill.scheduleRunsPerDay;
  return cap == null ? cron : '$cron · ≤$cap/day';
}

/// Tiny pill on a scheduled skill: the runner fires it on its own cadence
/// (references/11), so the catalogue says so. Unscheduled skills render
/// nothing. Carries the stable `skill-schedule:<id>` semantics label — the
/// selector contract, like `skill-layer:<id>`; do not rename casually.
class ScheduleBadge extends StatelessWidget {
  const ScheduleBadge({super.key, required this.skill});

  final SkillSummary skill;

  @override
  Widget build(BuildContext context) {
    final label = scheduleLabel(skill);
    if (label == null) return const SizedBox.shrink();
    return Semantics(
      label: 'skill-schedule:${skill.id}',
      identifier: 'skill-schedule:${skill.id}',
      container: true,
      explicitChildNodes: true,
      child: Tooltip(
        message: 'scheduled: the runner fires this skill on its cron',
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
          decoration: BoxDecoration(
            color: kSurfaceContainerHighest,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(Icons.schedule, size: 9, color: kOnSurfaceVariant),
              const SizedBox(width: 3),
              Text(
                label,
                style: Theme.of(context).textTheme.labelSmall?.copyWith(
                  color: kOnSurfaceVariant,
                  fontSize: 9,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
