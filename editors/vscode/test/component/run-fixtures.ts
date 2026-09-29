import type { RunView } from '../../src/shared/protocol';
import lineage from '../unit/fixtures/lineage/lineage-cascade.json';
import runEvents from '../unit/fixtures/lineage/run-events.json';
import callsPage1 from '../unit/fixtures/lineage/run-tool-calls-page1.json';
import callsPage2 from '../unit/fixtures/lineage/run-tool-calls-page2.json';

const run = lineage.nodes.find((node) => node.type === 'run');
const attemptEvent = runEvents.events.find(
  (event) => event.label_skill === 'escurel:run' && event.title === 'run-attempt',
);
const finishedEvent = runEvents.events.find(
  (event) => event.label_skill === 'escurel:run' && event.title === 'run-finished',
);
if (!run || !attemptEvent || !finishedEvent) throw new Error('Recorded run fixture is incomplete');

const attempt = JSON.parse(attemptEvent.body) as {
  attempt: number;
  started_at: string;
  ended_at: string;
  outcome: string;
};
const finished = JSON.parse(finishedEvent.body) as { tool_calls: number };

// These recordings are separate runs with the same echo harness shape.
export const recordedRunView: RunView = {
  runId: run.id,
  status: run.state,
  tone: 'run',
  harness: run.harness,
  autonomy: run.autonomy,
  targetPageId: run.target_page_id,
  traceId: run.trace_id,
  startedAt: run.started_at,
  finishedAt: run.finished_at,
  depth: run.depth ?? undefined,
  maxAttempts: run.max_attempts,
  attempts: [
    {
      n: attempt.attempt,
      startedAt: attempt.started_at,
      endedAt: attempt.ended_at,
      outcome: attempt.outcome,
    },
  ],
  plan: (run.plan ?? []).map((step) => ({
    step: step.step,
    status: step.status as RunView['plan'][number]['status'],
  })),
  summary: run.summary,
  toolCallCount: finished.tool_calls,
  calls: callsPage1.calls.map((call) => ({
    seq: call.seq,
    tool: call.tool,
    status: call.status,
    errorCode: call.error_code,
    durationMs: call.duration_ms,
    bytes: { request: call.request_bytes, response: call.response_bytes },
    at: call.at,
  })),
  nextAfter: callsPage1.next_after,
};

export const recordedLastPage: RunView = {
  ...recordedRunView,
  calls: [
    ...recordedRunView.calls,
    ...callsPage2.calls.map((call) => ({
      seq: call.seq,
      tool: call.tool,
      status: call.status,
      errorCode: call.error_code,
      durationMs: call.duration_ms,
      bytes: { request: call.request_bytes, response: call.response_bytes },
      at: call.at,
    })),
  ],
  nextAfter: callsPage2.next_after,
};
