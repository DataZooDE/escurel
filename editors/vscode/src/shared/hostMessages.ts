// A webview is a separate, less-trusted context. The host acts on the ids a webview posts, so it checks
// each against what the host itself put on that page, the way `resolvePageMessage` does for a page and
// `resolveRunAction` for a run. This is the skill page's. Pure.
import type { SkillPageModel, SkillPageToHost } from './skillPage';

/** Whether the skill page may be asked to act on this message, given the model the host last posted. */
export function skillPageMessageAllowed(
  model: SkillPageModel | undefined,
  m: SkillPageToHost,
): boolean {
  switch (m.type) {
    case 'ready':
    case 'refresh':
    case 'show-raw':
      return true;
    case 'start-skill':
      return (
        (m.mode === 'run' || m.mode === 'plan') &&
        !!model?.actions.some((a) => a.skill === m.skill)
      );
    case 'open-page':
      return (
        !!model &&
        (model.instances.items.some((i) => i.pageId === m.pageId) ||
          model.runs.some((r) => r.pageId === m.pageId))
      );
    case 'open-thread':
      return !!model?.runs.some((r) => r.rootEventId === m.rootEventId);
    case 'open-run':
      return !!model?.runs.some((r) => r.runId === m.runId);
  }
}
