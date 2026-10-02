import { randomBytes } from 'node:crypto';
import type { MintAgentTokenResponse } from '../client/types';

type RandomSource = (size: number) => Uint8Array;

function nonzeroHex(size: number, random: RandomSource): string {
  for (;;) {
    const bytes = random(size);
    if (bytes.length !== size) throw new Error('random source returned the wrong number of bytes');
    if (bytes.some((byte) => byte !== 0)) return Buffer.from(bytes).toString('hex');
  }
}

export function newTraceId(random: RandomSource = randomBytes): string {
  return nonzeroHex(16, random);
}

export function newSpanId(random: RandomSource = randomBytes): string {
  return nonzeroHex(8, random);
}

export function canStartInTerminal(input: {
  trusted: boolean;
  command: string;
}): { ok: true } | { ok: false; reason: string } {
  if (!input.trusted)
    return {
      ok: false,
      reason: 'Starting a harness in a terminal is disabled in an untrusted workspace.',
    };
  if (!input.command.trim())
    return {
      ok: false,
      reason: 'Set escurel.shellHarness to a command before starting in a terminal.',
    };
  return { ok: true };
}

export function buildTerminalSpec(input: {
  skill: string;
  pageId: string;
  gatewayUrl: string;
  command: string;
  mint: MintAgentTokenResponse;
  traceId: string;
  spanId: string;
}): { name: string; env: Record<string, string>; command: string } {
  const slug =
    input.pageId.split('/').pop()?.replace(/\.md$/, '').split('__').pop() ?? input.pageId;
  return {
    name: `escurel: ${input.skill} · ${slug}`,
    env: {
      ESCUREL_URL: input.gatewayUrl.replace(/\/+$/, ''),
      ESCUREL_TOKEN: input.mint.token,
      TRACEPARENT: `00-${input.traceId}-${input.spanId}-01`,
    },
    command: input.command,
  };
}
