import { describe, expect, it } from 'vitest';
import { EscurelError } from '../../src/client/errors';
import { decodeTranscript, transcriptFailure, transcriptRequest } from '../../src/runs/transcript';

const b64 = (s: string) => Buffer.from(s, 'utf8').toString('base64');

// The wire (put_run_transcript, references/02): JSONL, one `{ts, attempt, kind, name?, args_digest?,
// summary}` per line, kind one of tool_call / tool_result / message / error / command.
const JSONL = [
  { ts: '2026-11-13T06:00:00.000Z', attempt: 1, kind: 'message', summary: 'Reading the order.' },
  {
    ts: '2026-11-13T06:00:01.500Z',
    attempt: 1,
    kind: 'tool_call',
    name: 'expand',
    args_digest: 'sha256:abc',
    summary: '{"page_id":"markdown/instances/order/o1.md"}',
  },
  {
    ts: '2026-11-13T06:00:02.000Z',
    attempt: 1,
    kind: 'tool_result',
    name: 'expand',
    summary: 'ok',
  },
  { ts: '2026-11-13T06:01:05.000Z', attempt: 2, kind: 'error', summary: 'timeout' },
]
  .map((r) => JSON.stringify(r))
  .join('\n');

describe('run transcript', () => {
  it('decodes the JSONL into time-ordered records with offsets from the first one', () => {
    const records = decodeTranscript(b64(JSONL));
    expect(records.map((r) => r.kind)).toEqual(['message', 'tool_call', 'tool_result', 'error']);
    expect(records.map((r) => r.offsetMs)).toEqual([0, 1500, 2000, 65_000]);
    expect(records[1]).toMatchObject({ i: 1, name: 'expand', attempt: 1, digest: 'sha256:abc' });
    expect(records[3]).toMatchObject({ attempt: 2, summary: 'timeout' });
  });

  it('keeps a shell command the agent ran (codex) as its own kind', () => {
    const line = JSON.stringify({
      ts: '2026-11-13T06:00:00Z',
      attempt: 1,
      kind: 'command',
      summary: "/bin/bash -lc 'ls' (exit 0)",
    });
    expect(decodeTranscript(b64(line))).toEqual([
      { i: 0, kind: 'command', attempt: 1, summary: "/bin/bash -lc 'ls' (exit 0)", offsetMs: 0 },
    ]);
  });

  it('keeps the order the runner wrote when timestamps tie or are missing', () => {
    const rows = [
      { ts: '2026-11-13T06:00:02Z', attempt: 1, kind: 'message', summary: 'b' },
      { ts: '2026-11-13T06:00:01Z', attempt: 1, kind: 'message', summary: 'a' },
      { attempt: 1, kind: 'message', summary: 'c' },
    ];
    const records = decodeTranscript(b64(rows.map((r) => JSON.stringify(r)).join('\n')));
    expect(records.map((r) => r.summary)).toEqual(['a', 'b', 'c']);
    expect(records[2]!.offsetMs).toBeUndefined();
  });

  it('drops lines it cannot read and an unknown kind, never the whole transcript', () => {
    const text = ['not json', JSON.stringify({ kind: 'telemetry', summary: 'x' }), '', JSONL].join(
      '\n',
    );
    expect(decodeTranscript(b64(text))).toHaveLength(4);
  });

  it('cleans what the agent wrote: control characters out, bounded', () => {
    const line = JSON.stringify({
      ts: '2026-11-13T06:00:00Z',
      attempt: 1,
      kind: 'message',
      name: 'x\u0007y',
      summary: `evil\u001b[31m${'z'.repeat(500)}`,
    });
    const [r] = decodeTranscript(b64(line));
    expect(r!.name).toBe('xy');
    expect(r!.summary).not.toContain('\u001b');
    expect(r!.summary.length).toBeLessThanOrEqual(400);
  });

  it('decodes multi-byte text', () => {
    const line = JSON.stringify({ attempt: 1, kind: 'message', summary: 'Größe 5 €' });
    expect(decodeTranscript(b64(line))[0]!.summary).toBe('Größe 5 €');
  });

  it('maps the gateway refusals to honest states', () => {
    const refusal = (code: string) =>
      new EscurelError('rpc', `fetch_blob: ${code}`, { data: { code } });
    expect(transcriptFailure(refusal('transcript_absent'))).toEqual({
      status: 'absent',
      message: 'This run kept no transcript.',
    });
    expect(transcriptFailure(refusal('transcript_expired'))).toEqual({
      status: 'expired',
      message: 'This run’s transcript has been deleted: it is past the retention period.',
    });
    expect(transcriptFailure(refusal('run_not_found'))).toEqual({
      status: 'not-found',
      message: 'No finished run you may read has this id.',
    });
    expect(transcriptFailure(new Error('socket hang up'))).toEqual({
      status: 'error',
      message: 'socket hang up',
    });
  });

  it('knows when there is nothing to fetch yet, or at all', () => {
    expect(transcriptRequest({ status: 'running' })).toEqual({
      status: 'pending',
      message: 'The transcript is stored when the run finishes.',
    });
    expect(transcriptRequest({ status: 'processed' })).toEqual({
      status: 'absent',
      message: 'This run kept no transcript. Its agent does not record one.',
    });
    expect(transcriptRequest({ status: 'processed', transcript: { records: 3, bytes: 90 } })).toBe(
      'fetch',
    );
  });
});
