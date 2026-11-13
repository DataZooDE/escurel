import type { Event, GetRunToolCallsResponse, LineageNode } from '../../src/client';
import type { RunView, TranscriptRecord } from '../../src/shared/protocol';
import { buildRunView, mergeToolCallPage } from '../../src/runs/runModel';
import lineage from '../unit/fixtures/lineage/lineage-cascade.json';
import runEvents from '../unit/fixtures/lineage/run-events.json';
import callsPage1 from '../unit/fixtures/lineage/run-tool-calls-page1.json';
import callsPage2 from '../unit/fixtures/lineage/run-tool-calls-page2.json';

const run = (lineage.nodes as LineageNode[]).find((node) => node.type === 'run');
if (!run) throw new Error('Recorded run fixture is incomplete');

// The lineage node and event/call recordings were captured separately from the same run shape.
const base = buildRunView(run, runEvents.events as Event[]);
export const recordedRunView = mergeToolCallPage(base, callsPage1 as GetRunToolCallsResponse);
export const recordedLastPage = mergeToolCallPage(
  recordedRunView,
  callsPage2 as GetRunToolCallsResponse,
);

/** A live echo run that may be steered, with one message already sent (shape of `run-inject`). */
export const liveSteerableRun: RunView = {
  ...recordedRunView,
  status: 'running',
  tone: 'run',
  harness: 'echo',
  steer: { available: true },
  injections: [
    {
      n: 1,
      text: 'use the 2027 terms,\nnot 2026',
      requestedBy: 'ops:jo',
      at: '2026-11-13T06:00:04.000Z',
    },
  ],
};

/** A finished run that reported what it cost and stored a transcript (`run-finished.usage`, `.transcript`). */
export const costedRun: RunView = {
  ...recordedRunView,
  usage: { inputTokens: 12_400, outputTokens: 860, costUsd: 0.0421, model: 'claude-sonnet-4' },
  transcript: { records: 5, bytes: 900 },
};

/** Decoded transcript rows as `decodeTranscript` hands them over (src/runs/transcript.ts). */
export const transcriptRecords: TranscriptRecord[] = [
  { i: 0, kind: 'message', attempt: 1, summary: 'Reading the order first.', offsetMs: 0 },
  {
    i: 1,
    kind: 'tool_call',
    attempt: 1,
    name: 'expand',
    digest: 'sha256:9f2c',
    summary: '{"page_id":"markdown/instances/order/o1.md"}',
    offsetMs: 1500,
  },
  {
    i: 2,
    kind: 'tool_result',
    attempt: 1,
    name: 'expand',
    summary: '{"page":{"id":"o1"}}',
    offsetMs: 2100,
  },
  { i: 3, kind: 'error', attempt: 1, summary: 'update_page: agent_scope_denied', offsetMs: 4000 },
  {
    i: 4,
    kind: 'message',
    attempt: 2,
    summary: 'Retrying with the note page only.',
    offsetMs: 65_000,
  },
];
