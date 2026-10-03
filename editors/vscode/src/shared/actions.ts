import type { SkillAction } from '../client/types';
import type { ActionView } from './protocol';

/**
 * The Skill buttons a skill offers, from its `actions:` objects.
 *
 * Only `kind: event` can be started from the workbench: it is filed as an event under the skill
 * the action names, which is exactly what the start flow does. A `prompt` action is a chat turn
 * in another app and is not shown. The label is the skill author's own; a missing one is not
 * papered over with a name derived from an id, the action is skipped.
 */
export function skillActionViews(actions: readonly SkillAction[] | undefined): ActionView[] {
  const out: ActionView[] = [];
  for (const a of actions ?? []) {
    if (a.kind !== 'event' || !a.event || !a.label.trim()) continue;
    out.push({ skill: a.event, label: a.label });
  }
  return out;
}
