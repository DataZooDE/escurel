// Steering a live run (#820): pure rules shared by the host and the run-detail webview (no `vscode`).
import { describeOutcome, type ControlResult } from './controlResult';
import type { SendOutcome } from './controlWait';

/** The runner refuses an inject whose text is over this many bytes (references/11). */
export const INJECT_MAX_BYTES = 8192;

/**
 * Harnesses whose live session reads one more user turn. The runner is the authority: it answers
 * `unsupported` for any other, and that answer is still handled. This list only spares the person a
 * box that is bound to fail.
 */
const TAKES_MESSAGES = new Set(['claude', 'echo']);

export function harnessTakesMessages(harness: string | undefined): boolean {
  return TAKES_MESSAGES.has((harness ?? '').trim().toLowerCase());
}

/** Whether the steer box is shown: `undefined` hides it, `available: false` explains why it is not offered. */
export type SteerOffer = { available: true } | { available: false; reason: string };

export function steerOffer(run: {
  status: string;
  harness?: string | undefined;
}): SteerOffer | undefined {
  if (run.status !== 'running') return undefined;
  const harness = run.harness?.trim();
  if (!harness)
    return {
      available: false,
      reason: 'This run does not say which agent runs it, so it cannot be steered from here.',
    };
  return harnessTakesMessages(harness)
    ? { available: true }
    : { available: false, reason: `The ${harness} agent cannot take a message while it runs.` };
}

/** Why `text` would be refused, in words; `undefined` when it may be sent. */
export function injectTextProblem(text: string): string | undefined {
  if (!text.trim()) return 'Write a message first.';
  if (new TextEncoder().encode(text).length > INJECT_MAX_BYTES)
    return 'The message is too long: at most 8 KiB.';
  return undefined;
}

/** What the person is told after a steer, and whether the box may be cleared (the runner took it). */
export interface SteerReply {
  sent: boolean;
  message: string;
}

/** The runner's answer to a steer, in words. Only `injected` means the run will read the message. */
export function steerReply(outcome: SendOutcome<ControlResult>): SteerReply {
  switch (outcome.kind) {
    case 'refused':
      return { sent: false, message: outcome.message };
    case 'result':
      return {
        sent: outcome.result.outcome === 'injected',
        message: describeOutcome(outcome.result),
      };
    case 'timeout':
      // The request is stored, so the runner may still act on it: the box is cleared, not kept for a resend.
      return {
        sent: true,
        message: outcome.lookupFailed
          ? 'Sent, but the answer could not be read. The run shows the message if it arrived.'
          : 'Sent. The runner has not answered yet; the run shows the message when it arrives.',
      };
    case 'cancelled':
      return { sent: false, message: 'Not sent.' };
  }
}
