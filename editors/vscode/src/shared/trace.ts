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
  sizes: string;
}

export function traceTimeline(
  calls: readonly ToolCallRow[],
  startedAt: string | undefined,
): TraceRow[] {
  const start = parseGatewayTime(startedAt)?.getTime();
  const slowest = calls.reduce((m, c) => Math.max(m, c.durationMs), 0);
  return calls.map((c) => {
    const at = parseGatewayTime(c.at)?.getTime();
    const failed = c.status === 'error' || c.status === 'rejected';
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
      barPercent: slowest > 0 ? Math.round((c.durationMs / slowest) * 100) : 0,
      sizes: `sent ${formatBytes(c.bytes.request)} · received ${formatBytes(c.bytes.response)}`,
    };
  });
}
