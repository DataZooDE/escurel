import type { Event, GetRunToolCallsResponse, LineageNode } from '../../src/client';
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
