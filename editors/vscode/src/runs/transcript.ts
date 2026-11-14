import { EscurelError } from '../client/errors';
import type { EscurelClient } from '../client';
import type {
  RunView,
  TranscriptKind,
  TranscriptRecord,
  TranscriptState,
} from '../shared/protocol';
import { describeError } from '../errors';
import { parseGatewayTime } from '../shared/time';
import { cleanText } from '../shared/untrustedText';

const KINDS = new Set<TranscriptKind>(['tool_call', 'tool_result', 'message', 'error', 'command']);
/** The runner keeps at most 4000 records per run; anything past that is not a transcript it wrote. */
const MAX_RECORDS = 4000;

/**
 * A run transcript (`fetch_blob{run_id}`, JSONL) as rows to replay. Every field was written by the
 * agent's harness, so each is cleaned and bounded here, once. A line that cannot be read is dropped,
 * never the whole transcript. Rows are ordered by time; ties and rows without a time keep the order
 * the runner wrote them in.
 */
export function decodeTranscript(base64: string): TranscriptRecord[] {
  const text = Buffer.from(base64, 'base64').toString('utf8');
  const rows: (TranscriptRecord & { at?: number; line: number })[] = [];
  for (const [line, raw] of text.split('\n').entries()) {
    if (rows.length >= MAX_RECORDS) break;
    if (!raw.trim()) continue;
    let value: unknown;
    try {
      value = JSON.parse(raw);
    } catch {
      continue;
    }
    if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
    const r = value as Record<string, unknown>;
    if (!KINDS.has(r.kind as TranscriptKind)) continue;
    const at = parseGatewayTime(r.ts)?.getTime();
    const name = typeof r.name === 'string' ? cleanText(r.name, 120) : '';
    const digest = typeof r.args_digest === 'string' ? cleanText(r.args_digest, 80) : '';
    rows.push({
      i: 0,
      kind: r.kind as TranscriptKind,
      attempt: typeof r.attempt === 'number' && Number.isFinite(r.attempt) ? r.attempt : 1,
      summary: typeof r.summary === 'string' ? cleanText(r.summary, 400) : '',
      ...(name ? { name } : {}),
      ...(digest ? { digest } : {}),
      ...(at !== undefined ? { at } : {}),
      line,
    });
  }
  const timeOf = (r: { at?: number }) => r.at ?? Number.POSITIVE_INFINITY;
  rows.sort((a, b) => timeOf(a) - timeOf(b) || a.line - b.line);
  const first = rows.find((r) => r.at !== undefined)?.at;
  return rows.map(({ at, line: _line, ...r }, i) => ({
    ...r,
    i,
    ...(at !== undefined && first !== undefined ? { offsetMs: at - first } : {}),
  }));
}

const REFUSALS: Record<string, Exclude<TranscriptState, { status: 'ready' | 'loading' }>> = {
  transcript_absent: { status: 'absent', message: 'This run kept no transcript.' },
  transcript_expired: {
    status: 'expired',
    message: 'This run’s transcript has been deleted: it is past the retention period.',
  },
  run_not_found: { status: 'not-found', message: 'No finished run you may read has this id.' },
};

/** A failed read in words: the gateway names why with `data.code`. */
export function transcriptFailure(error: unknown): TranscriptState {
  const code =
    error instanceof EscurelError
      ? (error.data as { code?: unknown } | undefined)?.code
      : undefined;
  const known = typeof code === 'string' ? REFUSALS[code] : undefined;
  return known ?? { status: 'error', message: describeError(error) };
}

/** Whether there is a transcript to fetch, from what the run itself says; otherwise the state to show. */
export function transcriptRequest(
  run: Pick<RunView, 'status' | 'transcript'>,
): TranscriptState | 'fetch' {
  if (run.status === 'running')
    return { status: 'pending', message: 'The transcript is stored when the run finishes.' };
  if (!run.transcript)
    return {
      status: 'absent',
      message: 'This run kept no transcript. Its agent does not record one.',
    };
  return 'fetch';
}

/** The transcript of `runId`, or the honest state when there is none to show. */
export async function loadTranscript(
  client: EscurelClient,
  run: Pick<RunView, 'runId' | 'status' | 'transcript'>,
): Promise<TranscriptState> {
  const ask = transcriptRequest(run);
  if (ask !== 'fetch') return ask;
  try {
    const { blob } = await client.fetchRunTranscript(run.runId);
    if (!blob) return REFUSALS.transcript_absent!;
    return { status: 'ready', records: decodeTranscript(blob.bytes_base64) };
  } catch (error) {
    return transcriptFailure(error);
  }
}
