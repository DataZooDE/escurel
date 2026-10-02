import type { PageModel, StartMode, WebviewToHost } from '../shared/protocol';

const MODES: readonly StartMode[] = ['background', 'plan', 'terminal'];

export interface PageCommand {
  command: string;
  args: unknown[];
}

const nonEmpty = (v: unknown): v is string => typeof v === 'string' && v !== '';

/**
 * What the host will do for a message from the page webview, decided from the model THE HOST built.
 *
 * The webview is not trusted: it can post anything. A `start-skill` is a write (it captures an
 * event; in terminal mode it mints a token), so it is accepted only for a skill this page offers
 * and one of the three modes, and always on THIS page. The opens are reads, but the ids they pass on
 * must at least be strings.
 */
export function resolvePageMessage(
  model: PageModel | undefined,
  m: WebviewToHost,
): PageCommand | undefined {
  switch (m.type) {
    case 'start-skill': {
      if (!model || !MODES.includes(m.mode)) return undefined;
      if (!model.actions.some((a) => a.skill === m.skill)) return undefined;
      return {
        command: 'escurel.startSkill',
        args: [{ skill: m.skill, pageId: model.pageId, mode: m.mode }],
      };
    }
    case 'view-skill': {
      if (!model || !nonEmpty(m.skill)) return undefined;
      const known = m.skill === model.skill.id || model.actions.some((a) => a.skill === m.skill);
      return known ? { command: 'escurel.viewSkill', args: [m.skill] } : undefined;
    }
    case 'open-run':
      return nonEmpty(m.runId) ? { command: 'escurel.openRun', args: [m.runId] } : undefined;
    case 'open-thread':
      return nonEmpty(m.rootEventId)
        ? { command: 'escurel.openThread', args: [m.rootEventId] }
        : undefined;
    case 'open-page':
      return nonEmpty(m.pageId) ? { command: 'escurel.openPage', args: [m.pageId] } : undefined;
    case 'open-wikilink':
      return nonEmpty(m.wikilink) ? { command: 'escurel.resolve', args: [m.wikilink] } : undefined;
    default:
      return undefined;
  }
}
