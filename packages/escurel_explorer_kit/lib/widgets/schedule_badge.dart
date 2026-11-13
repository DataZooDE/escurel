import 'package:flutter/material.dart';

import '../client/models.dart';
import '../theme/app_theme.dart';

/// Whether the runner fires nothing for this schedule: the gateway flags the
/// expression or the budget as unreadable (references/11).
bool scheduleBlocked(SkillSummary skill) =>
    skill.scheduleInvalid || skill.scheduleBudgetInvalid;

/// "0 9 * * 1-5 · ≤3/day" — a skill's `schedule:` cron as written, plus its
/// `schedule_budget.runs_per_day` cap when one is set; "Schedule blocked ·
/// `cron`" when the runner fires nothing. Null when the skill is not
/// scheduled.
String? scheduleLabel(SkillSummary skill) {
  final cron = skill.schedule;
  if (cron == null || cron.trim().isEmpty) return null;
  if (scheduleBlocked(skill)) return 'Schedule blocked · $cron';
  final cap = skill.scheduleRunsPerDay;
  return cap == null ? cron : '$cron · ≤$cap/day';
}

/// The badge's tooltip: what the runner does with this schedule.
String scheduleTooltip(SkillSummary skill) {
  if (skill.scheduleInvalid) {
    return 'Schedule blocked: the schedule cannot be read, so the runner '
        'fires nothing. Fix the expression; validate names the problem.';
  }
  if (skill.scheduleBudgetInvalid) {
    return 'Schedule blocked: schedule_budget.runs_per_day must be a positive '
        'integer, so the runner fires nothing.';
  }
  return 'scheduled: the runner fires this skill on its cron';
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
        message: scheduleTooltip(skill),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
          decoration: BoxDecoration(
            color: kSurfaceContainerHighest,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                scheduleBlocked(skill) ? Icons.block : Icons.schedule,
                size: 9,
                color: kOnSurfaceVariant,
              ),
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
