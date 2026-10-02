import type { LineageNode } from '../client';

export type PlanWatchResult =
  | { state: 'planned'; runId: string; node: LineageNode }
  | { state: 'failed'; runId?: string; reason: string; node?: LineageNode }
  | { state: 'timeout' }
  | { state: 'cancelled' };

export interface PlanWatchOptions {
  rootEventId: string;
  fetchLineage: (rootEventId: string) => Promise<{ nodes: LineageNode[] }>;
  sleep?: (ms: number) => Promise<void>;
  pollIntervalMs?: number;
  timeoutMs?: number;
  isCancelled?: () => boolean;
  now?: () => number;
}

/**
 * Pure decision machine that polls the lineage for a root event until a run
 * node reaches 'planned' or a terminal failure, or times out/cancels.
 *
 * Polling defaults to every 750 ms, at most 5 minutes.
 */
export async function watchPlan(options: PlanWatchOptions): Promise<PlanWatchResult> {
  const {
    rootEventId,
    fetchLineage,
    sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    pollIntervalMs = 750,
    timeoutMs = 5 * 60 * 1000,
    isCancelled = () => false,
    now = () => Date.now(),
  } = options;

  const startTime = now();

  while (true) {
    if (isCancelled()) {
      return { state: 'cancelled' };
    }

    if (now() - startTime >= timeoutMs) {
      return { state: 'timeout' };
    }

    try {
      const lineage = await fetchLineage(rootEventId);
      const runNode = lineage.nodes.find((n) => n.type === 'run');

      if (runNode) {
        if (runNode.state === 'planned') {
          return { state: 'planned', runId: runNode.id, node: runNode };
        }
        if (runNode.state === 'cancelled') {
          // A cancelled plan run is over; polling on to the timeout would end in a misleading
          // "timed out" warning for a run somebody stopped.
          return {
            state: 'failed',
            runId: runNode.id,
            reason: 'The plan run was cancelled.',
            node: runNode,
          };
        }
        if (runNode.state === 'failed' || runNode.state === 'dead_letter') {
          const reason =
            (runNode.summary as string | undefined) ||
            (runNode.reason as string | undefined) ||
            (runNode.error as string | undefined) ||
            `Run reached state ${runNode.state}`;
          return { state: 'failed', runId: runNode.id, reason, node: runNode };
        }
      }
    } catch {
      // Network/gateway glitch: keep trying until timeout unless cancelled
    }

    if (isCancelled()) {
      return { state: 'cancelled' };
    }

    const elapsed = now() - startTime;
    if (elapsed >= timeoutMs) {
      return { state: 'timeout' };
    }

    const delay = Math.min(pollIntervalMs, timeoutMs - elapsed);
    await sleep(delay);
  }
}
