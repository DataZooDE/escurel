import { pageSlug } from '../shared/pageId';

interface Titled {
  runId: string;
  skill?: string | undefined;
  targetPageId?: string | undefined;
}

const shortId = (runId: string): string => runId.slice(-6);

/** What a person calls a run: the skill that ran and the page it ran on. The id is secondary. */
export function runHeading(run: Titled): { title: string; id: string } {
  const target = run.targetPageId ? pageSlug(run.targetPageId) : '';
  const title =
    run.skill && target ? `${run.skill} on ${target}` : run.skill || `Run ${shortId(run.runId)}`;
  return { title, id: run.runId };
}

export function runTabTitle(run: Titled): string {
  const target = run.targetPageId ? pageSlug(run.targetPageId) : '';
  if (run.skill && target) return `Run · ${run.skill} · ${target}`;
  return run.skill ? `Run · ${run.skill}` : `Run · ${shortId(run.runId)}`;
}

const FINISHED = new Set(['processed', 'failed', 'dead_letter', 'cancelled']);

export type DisplayedStepStatus =
  'pending' | 'in_progress' | 'completed' | 'blocked' | 'unfinished';

/** A run that has finished is not still doing a step: show it as not finished. */
export function displayStepStatus(
  step: 'pending' | 'in_progress' | 'completed' | 'blocked',
  runStatus: string,
): DisplayedStepStatus {
  return step === 'in_progress' && FINISHED.has(runStatus) ? 'unfinished' : step;
}
