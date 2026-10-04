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

export interface TraceRow {
  seq: number;
  tool: string;
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
