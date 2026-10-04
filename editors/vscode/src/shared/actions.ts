import type { SkillAction } from '../client/types';
import type { ActionView } from './protocol';
import { cleanText } from './untrustedText';

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
    // The label is the skill author's own words; the skill it starts is shown beside it, so a friendly
    // label cannot hide what the button starts.
    out.push({ skill: cleanText(a.event, 100), label: cleanText(a.label, 80) });
  }
  return out;
}
