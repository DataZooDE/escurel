import { describe, expect, it } from 'vitest';
import { buildControlEvent } from '../../src/runs/controls';
import { describeOutcome } from '../../src/runs/controlResult';
import { outcomeChannel, progressTitle } from '../../src/runs/controlWording';
import {
  INJECT_MAX_BYTES,
  harnessTakesMessages,
  injectTextProblem,
  steerOffer,
  steerReply,
} from '../../src/runs/steer';

// The runner's contract (references/11, "What an inject does"): the runtime page says which harness
// reads a message mid-run; the text is non-blank and at most 8 KiB; the result is injected | unsupported |
// not_live.
describe('steering a live run', () => {
  // The runtime holding the run says, per harness, whether it takes a message (#827): no guess here.
  const heldBy = (harnesses: { name: string; supports_inject?: boolean }[]) => ({
    runtime: 'muc',
    until: '2026-11-14T03:00:00Z',
    harnesses,
  });

  it('reads whether the harness takes a message from the runtime that holds the run', () => {
    const held = heldBy([
      { name: 'echo', supports_inject: true },
      { name: 'codex', supports_inject: false },
      { name: 'claude' },
    ]);
    expect(harnessTakesMessages(held, 'echo')).toBe(true);
    expect(harnessTakesMessages(held, ' Echo ')).toBe(true);
    expect(harnessTakesMessages(held, 'codex')).toBe(false);
    // An older runner's page does not say: unknown, not "no".
    expect(harnessTakesMessages(held, 'claude')).toBeUndefined();
    expect(harnessTakesMessages(held, 'gemini')).toBeUndefined();
    expect(harnessTakesMessages(undefined, 'echo')).toBeUndefined();
  });

  it('offers the steer box only while the run is live and its runtime says the harness takes messages', () => {
    const held = heldBy([
      { name: 'echo', supports_inject: true },
      { name: 'codex', supports_inject: false },
    ]);
    expect(steerOffer({ status: 'running', harness: 'echo', heldBy: held })).toEqual({
      available: true,
    });
    expect(steerOffer({ status: 'processed', harness: 'echo', heldBy: held })).toBeUndefined();
    expect(steerOffer({ status: 'planned', harness: 'claude' })).toBeUndefined();
    // Live, but the harness cannot read a message: say so instead of offering a box that fails.
    expect(steerOffer({ status: 'running', harness: 'codex', heldBy: held })).toEqual({
      available: false,
      reason: 'The codex agent cannot take a message while it runs.',
    });
    expect(steerOffer({ status: 'running' })).toEqual({
      available: false,
      reason: 'This run does not say which agent runs it, so it cannot be steered from here.',
    });
  });

  it('offers the box with a hint when no runtime says, since the runner still answers safely', () => {
    const unknown = {
      available: true,
      hint: 'Whether the claude agent can take a message is not known here; the runner says if it cannot.',
    };
    expect(steerOffer({ status: 'running', harness: 'claude' })).toEqual(unknown);
    expect(
      steerOffer({ status: 'running', harness: 'claude', heldBy: heldBy([{ name: 'claude' }]) }),
    ).toEqual(unknown);
  });

  it('refuses blank or oversized text the way the gateway would, counting UTF-8 bytes', () => {
    expect(injectTextProblem('use the 2027 terms')).toBeUndefined();
    expect(injectTextProblem('   ')).toBe('Write a message first.');
    expect(injectTextProblem('a'.repeat(INJECT_MAX_BYTES))).toBeUndefined();
    expect(injectTextProblem('a'.repeat(INJECT_MAX_BYTES + 1))).toMatch(/8 KiB/);
    // 3 bytes each: 2731 of them is 8193 bytes, one over, though far under 8192 characters.
    expect(injectTextProblem('€'.repeat(2731))).toMatch(/8 KiB/);
  });

  it('builds the inject control with the text whole', () => {
    const e = buildControlEvent({ action: 'inject', runId: '01RUN', text: ' use 2027\nterms ' });
    expect(e.label_skill).toBe('escurel:run-control');
    expect(JSON.parse(e.body!)).toEqual({
      action: 'inject',
      run_id: '01RUN',
      text: ' use 2027\nterms ',
    });
    expect(() => buildControlEvent({ action: 'inject', runId: '01RUN', text: ' ' })).toThrow(
      /message/,
    );
    expect(() => buildControlEvent({ action: 'inject', runId: '', text: 'x' })).toThrow(/run/);
  });

  it('words every inject outcome honestly', () => {
    const r = (outcome: string, detail?: string) => ({
      action: 'inject',
      outcome,
      runId: '01RUN',
      detail: detail ?? null,
    });
    expect(describeOutcome(r('injected', 'injection 1'))).toBe(
      'Message sent. The agent reads it as its next turn.',
    );
    expect(describeOutcome(r('unsupported', 'codex'))).toBe(
      'This agent (codex) cannot take a message while it runs.',
    );
    expect(describeOutcome(r('not_live', 'processed'))).toBe(
      'That run is not running any more (processed).',
    );
    expect(outcomeChannel('injected')).toBe('status');
    expect(outcomeChannel('unsupported')).toBe('toast');
    expect(progressTitle('inject')).toBe('Sending your message to the run…');
  });
});

describe('steerReply', () => {
  const result = (outcome: string, detail?: string) => ({
    kind: 'result' as const,
    result: { action: 'inject', outcome, runId: 'r', detail: detail ?? null },
  });
  it('clears the box only when the runner took the message, or may still', () => {
    expect(steerReply(result('injected'))).toEqual({
      sent: true,
      message: 'Message sent. The agent reads it as its next turn.',
    });
    expect(steerReply(result('unsupported', 'codex')).sent).toBe(false);
    expect(steerReply(result('not_live', 'processed')).sent).toBe(false);
    expect(
      steerReply({ kind: 'refused', message: 'No such run, or not yours to control.' }),
    ).toEqual({
      sent: false,
      message: 'No such run, or not yours to control.',
    });
    expect(steerReply({ kind: 'timeout', lookupFailed: false }).sent).toBe(true);
  });
});
