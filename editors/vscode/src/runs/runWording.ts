const AUTONOMY: Record<string, string> = {
  review: 'Its changes need your approval.',
  auto: 'Its changes are applied without review.',
  confirm: 'It asks you to confirm before it acts.',
};

/** Who ran it and what that means for its changes, as a sentence (not 'Harness echo · Autonomy review'). */
export function runByline(run: {
  harness?: string | undefined;
  model?: string | undefined;
  autonomy?: string | undefined;
  depth?: number | undefined;
}): string {
  const parts: string[] = [];
  if (run.harness)
    parts.push(`Run by the ${run.harness} agent${run.model ? ` (${run.model})` : ''}.`);
  if (run.autonomy && AUTONOMY[run.autonomy]) parts.push(AUTONOMY[run.autonomy]!);
  if (typeof run.depth === 'number' && run.depth > 0) parts.push(`Follow-up level ${run.depth}.`);
  return parts.join(' ');
}

const STARTING = new Set(['running', 'planned']);

/** 'No attempts reported' under a run that has only just begun reads as an error. */
export function emptyAttempts(status: string): string {
  return STARTING.has(status) ? 'Starting…' : 'No attempts were recorded.';
}

export function emptyPlan(status: string): string {
  return status === 'running' || status === 'planned'
    ? 'The agent has not reported a plan yet.'
    : 'No plan was recorded.';
}

/** An icon shape per state: a state is never told by its colour alone. */
export function statusIconName(status: string): 'check' | 'sync' | 'cross' | 'warn' {
  switch (status) {
    case 'processed':
      return 'check';
    case 'running':
      return 'sync';
    case 'failed':
    case 'dead_letter':
      return 'cross';
    default:
      return 'warn';
  }
}

/** A run's status as a person says it: `processed` is "done", `dead_letter` is "gave up". */
export function statusWord(status: string): string {
  switch (status) {
    case 'processed':
      return 'done';
    case 'dead_letter':
      return 'gave up';
    case 'planned':
      return 'plan ready';
    default:
      return status.replaceAll('_', ' ');
  }
}
