import type { ThreadNodeKind } from '../shared/protocol';

export interface SummaryFacts {
  kind: ThreadNodeKind;
  state?: string | undefined;
  drafts?: number | undefined;
  runs?: number | undefined;
  duration?: string | undefined;
  reason?: string | undefined;
  /** A short name of the page a draft changes. */
  target?: string | undefined;
}

export interface NodeSummary {
  text: string;
  /** True when a person has to act (review, approve, retry): the panel makes it stand out. */
  needsYou: boolean;
}

const plural = (n: number, one: string, many: string): string => (n === 1 ? one : many);

/**
 * One sentence for the Details panel: what happened to this node, and whether it waits for a
 * person. The raw fields (trace id, harness, attempts ...) stay available below it.
 */
export function nodeSummary(f: SummaryFacts): NodeSummary {
  const s = f.state ?? '';
  switch (f.kind) {
    case 'changeset': {
      const n = f.drafts ?? 0;
      if (s === 'open') {
        return {
          text: `${n} ${plural(n, 'change is', 'changes are')} waiting for your review.`,
          needsYou: true,
        };
      }
      if (s === 'promoted') {
        return {
          text: `Applied: ${n} ${plural(n, 'change was', 'changes were')} accepted.`,
          needsYou: false,
        };
      }
      if (s === 'discarded') return { text: 'Rejected: nothing was applied.', needsYou: false };
      return { text: 'A set of proposed changes.', needsYou: false };
    }
    case 'run': {
      if (s === 'running') return { text: 'The agent is working on it.', needsYou: false };
      if (s === 'planned') {
        return {
          text: 'The agent made a plan and is waiting for you to approve it.',
          needsYou: true,
        };
      }
      if (s === 'processed') {
        return {
          text: f.duration ? `The agent finished in ${f.duration}.` : 'The agent finished.',
          needsYou: false,
        };
      }
      if (s === 'failed' || s === 'dead_letter') {
        const why = f.reason ? `: ${f.reason}` : '';
        return {
          text: `The agent stopped and could not finish${why}. Retry it, or ask an admin.`,
          needsYou: true,
        };
      }
      if (s === 'cancelled') return { text: 'This run was cancelled.', needsYou: false };
      return { text: 'An agent run.', needsYou: false };
    }
    case 'event': {
      if (s === 'inbox') {
        return { text: 'A signal arrived and has not been handled yet.', needsYou: false };
      }
      if (s === 'processed') {
        const n = f.runs ?? 0;
        return {
          text:
            n > 0
              ? `This signal was handled. ${n} agent ${plural(n, 'run', 'runs')} followed.`
              : 'This signal was handled.',
          needsYou: false,
        };
      }
      return { text: 'A signal.', needsYou: false };
    }
    case 'draft': {
      const page = f.target ?? 'a page';
      if (s === 'open') {
        return { text: `A proposed change to ${page} is waiting for review.`, needsYou: true };
      }
      if (s === 'promoted') return { text: `The change to ${page} was applied.`, needsYou: false };
      if (s === 'discarded') {
        return { text: `The change to ${page} was rejected.`, needsYou: false };
      }
      return { text: `A proposed change to ${page}.`, needsYou: false };
    }
  }
}
