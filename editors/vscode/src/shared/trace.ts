// Pure wording for a run's tool-call trace: shared by the run detail webview and its tests.
import type { ToolCallRow } from './protocol';
import { parseGatewayTime } from './time';

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const round = (v: number) => String(Math.round(v * 10) / 10);
  if (n < 1024 * 1024) return `${round(n / 1024)} KB`;
  return `${round(n / (1024 * 1024))} MB`;
}

export function callDuration(ms: number): string {
  if (ms < 1) return '< 1 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${Math.round(s * 10) / 10} s`;
  const whole = Math.round(s);
  return `${Math.floor(whole / 60)} min ${whole % 60} s`;
}

const TOOL_WORDS: Record<string, string> = {
  list_inbox: 'Read the inbox',
  list_events: 'Read events',
  list_instances: 'Looked up records',
  list_skills: 'Read the skills',
  query_instance: 'Queried records',
  expand: 'Opened a page',
  read_page: 'Read a page',
  search: 'Searched',
  resolve: 'Followed a link',
  neighbours: 'Looked at related pages',
  create_draft: 'Proposed a change',
  update_page: 'Changed a page',
  capture_event: 'Filed an event',
  assign_event: 'Handed an event on',
  apply_op: 'Edited a page',
  close_session: 'Saved its edits',
  report_progress: 'Reported progress',
};

/** What a tool call did, in words ("Read the inbox"); the raw tool name stays in the tooltip. */
export function toolWords(tool: string): string {
  const known = TOOL_WORDS[tool];
  if (known) return known;
  const spaced = tool.replace(/[_-]+/g, ' ').trim();
  return spaced ? spaced.charAt(0).toUpperCase() + spaced.slice(1) : 'Tool call';
}

/** The gateway records how long each call took and how much it moved, not what was in it. */
export const TRACE_RECORDED_NOTE =
  'The gateway records the size and timing of each call, not its arguments or its result.';

/** When the gateway kept a summary of what each call asked and got back. */
export const TRACE_DETAIL_NOTE =
  'Open a step to see what it asked and what came back. Both are shortened, and credentials are removed.';

/** The note under the trace heading: which of the two the gateway recorded for these calls. */
export function traceNote(calls: readonly ToolCallRow[]): string {
  return calls.some((c) => c.argsSummary || c.resultSummary)
    ? TRACE_DETAIL_NOTE
    : TRACE_RECORDED_NOTE;
}

/** A summary for reading: pretty-printed when it is whole JSON, as written when it was cut. */
export function readableSummary(summary: string | undefined): string {
  if (!summary) return '';
  try {
    return JSON.stringify(JSON.parse(summary), null, 2);
  } catch {
    return summary;
  }
}

export interface TraceRow {
  seq: number;
  tool: string;
  /** What it did, in words. */
  label: string;
  /** `ok`, `failed`, `rejected`: a word, never colour alone. */
  outcome: string;
  failed: boolean;
  /** The error code, when there is one. */
  detail: string;
  /** Time since the run started ("+2 s"), empty when the start is unknown. */
  offset: string;
  duration: string;
  /** 0-100, the call's duration against the slowest call of the run. */
  barPercent: number;
  /** 0-100, where the call began on the run's time axis (0 when the run has no usable times). */
  leftPercent: number;
  /** 0-100, how much of the axis the call took; never so thin that it disappears. */
  widthPercent: number;
  sizes: string;
  /** What the call asked, readable; empty when the gateway kept none. */
  args: string;
  /** What came back (or why it failed), readable; empty when the gateway kept none. */
  result: string;
}

/** A tick or an end of the axis in the unit a person reads: "0", "250 ms", "1.5 s", "1 min 30 s". */
export function axisLabel(ms: number): string {
  if (ms <= 0) return '0';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${Math.round(ms / 100) / 10} s`;
  const whole = Math.round(ms / 1000);
  const min = Math.floor(whole / 60);
  const sec = whole % 60;
  return sec === 0 ? `${min} min` : `${min} min ${sec} s`;
}

export interface TraceAxis {
  /** From the run's start (or its first call) to the end of its last call. */
  totalMs: number;
  /** Round marks along the span, the first at 0. */
  ticks: { percent: number; label: string }[];
  endLabel: string;
}

const MIN_BAR_PERCENT = 1;
const MAX_TICK_PERCENT = 90;
const TICK_STEPS_MS = [
  1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000,
  300_000, 600_000, 1_800_000, 3_600_000,
];

interface Span {
  zero: number;
  totalMs: number;
}

/** The span the calls cover; undefined when no call carries a usable time. */
function traceSpan(calls: readonly ToolCallRow[], startedAt: string | undefined): Span | undefined {
  const starts = calls.map((c) => parseGatewayTime(c.at)?.getTime());
  if (calls.length === 0 || starts.some((t) => t === undefined)) return undefined;
  const first = Math.min(...(starts as number[]));
  const runStart = parseGatewayTime(startedAt)?.getTime();
  const zero = runStart === undefined ? first : Math.min(runStart, first);
  const end = Math.max(...calls.map((c, i) => (starts[i] as number) + c.durationMs));
  return { zero, totalMs: Math.max(end - zero, 1) };
}

/** The time axis above a run's calls: how long the run took and round marks along it. */
export function traceAxis(
  calls: readonly ToolCallRow[],
  startedAt: string | undefined,
): TraceAxis | undefined {
  const span = traceSpan(calls, startedAt);
  if (!span) return undefined;
  const step = TICK_STEPS_MS.find((s) => span.totalMs / s <= 5) ?? TICK_STEPS_MS.at(-1)!;
  const ticks: TraceAxis['ticks'] = [];
  // A mark close to the right end would sit on top of the end label.
  for (let t = 0; t <= span.totalMs; t += step) {
    const percent = (t / span.totalMs) * 100;
    if (percent <= MAX_TICK_PERCENT) ticks.push({ percent, label: axisLabel(t) });
  }
  return { totalMs: span.totalMs, ticks, endLabel: axisLabel(span.totalMs) };
}

export function traceTimeline(
  calls: readonly ToolCallRow[],
  startedAt: string | undefined,
): TraceRow[] {
  const start = parseGatewayTime(startedAt)?.getTime();
  const slowest = calls.reduce((m, c) => Math.max(m, c.durationMs), 0);
  const span = traceSpan(calls, startedAt);
  return calls.map((c) => {
    const at = parseGatewayTime(c.at)?.getTime();
    const failed = c.status === 'error' || c.status === 'rejected';
    const barPercent = slowest > 0 ? Math.round((c.durationMs / slowest) * 100) : 0;
    let leftPercent = 0;
    let widthPercent = barPercent;
    if (span && at !== undefined) {
      widthPercent = Math.min(100, Math.max((c.durationMs / span.totalMs) * 100, MIN_BAR_PERCENT));
      leftPercent = Math.min(((at - span.zero) / span.totalMs) * 100, 100 - widthPercent);
    }
    return {
      seq: c.seq,
      tool: c.tool,
      label: toolWords(c.tool),
      outcome: c.status === 'error' ? 'failed' : c.status,
      failed,
      detail: c.errorCode ?? '',
      offset:
        start !== undefined && at !== undefined
          ? `+${callDuration(Math.max(0, at - start))}`.replace('+< 1 ms', '+0 ms')
          : '',
      duration: callDuration(c.durationMs),
      barPercent,
      leftPercent,
      widthPercent,
      sizes: `sent ${formatBytes(c.bytes.request)} · received ${formatBytes(c.bytes.response)}`,
      args: readableSummary(c.argsSummary),
      result: readableSummary(c.resultSummary),
    };
  });
}
