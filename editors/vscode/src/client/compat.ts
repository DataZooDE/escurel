import { EscurelError } from './errors';
import type { Skill } from './types';

/**
 * A gateway from before skill `actions:` became objects answers `actions: ["supplier-risk"]` (plain skill
 * ids). Every view that renders from the catalogue would then crash (`t.replace is not a function`) and
 * leave a blank panel. Say so once, in words, at the boundary instead.
 */
export function checkSkillsCompatible(skills: Skill[]): Skill[] {
  for (const s of skills) {
    const actions = s.actions as unknown;
    if (Array.isArray(actions) && actions.some((a) => typeof a !== 'object' || a === null)) {
      throw new EscurelError(
        'server_incompatible',
        `skill ${String(s.id)} declares its actions in the old list-of-ids format`,
      );
    }
  }
  return skills;
}
