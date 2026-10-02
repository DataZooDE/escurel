import { describe, expect, it } from 'vitest';
import {
  buildTerminalSpec,
  canStartInTerminal,
  newSpanId,
  newTraceId,
} from '../../src/start/terminalSpec';

const token = 'DISTINCTIVE_SECRET_TOKEN_987';
const traceId = '1234567890abcdef1234567890abcdef';
const spanId = 'fedcba9876543210';
const command = 'node -e "process.exit(0)"';

describe('buildTerminalSpec', () => {
  it('sets only the three exact environment values and preserves the command', () => {
    const spec = buildTerminalSpec({
      skill: 'supplier-risk',
      pageId: 'markdown/instances/customer-order__order-4500123.md',
      gatewayUrl: 'https://gateway.example///',
      command,
      mint: {
        token,
        run_id: 'run-1',
        root_event_id: 'root-1',
        subject: 'alice',
        expires_at: 'later',
      },
      traceId,
      spanId,
    });
    expect(spec.env).toEqual({
      ESCUREL_URL: 'https://gateway.example',
      ESCUREL_TOKEN: token,
      TRACEPARENT: `00-${traceId}-${spanId}-01`,
    });
    expect(spec.env.TRACEPARENT).toMatch(/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
    expect(spec.name).toBe('escurel: supplier-risk · order-4500123');
    expect(spec.name).not.toContain(token);
    expect(spec.command).toBe(command);
    expect(spec.command).not.toContain(token);
  });
});

describe('terminal start guards', () => {
  it('refuses untrusted workspaces', () => {
    expect(canStartInTerminal({ trusted: false, command })).toEqual({
      ok: false,
      reason: 'Starting a harness in a terminal is disabled in an untrusted workspace.',
    });
  });

  it('refuses a blank harness command for a distinct reason', () => {
    expect(canStartInTerminal({ trusted: true, command: '  \n ' })).toEqual({
      ok: false,
      reason: 'Set escurel.shellHarness to a command before starting in a terminal.',
    });
    expect(canStartInTerminal({ trusted: true, command })).toEqual({ ok: true });
  });
});

describe('trace identifiers', () => {
  it('never returns all zero identifiers, even when the first random draw is zero', () => {
    let calls = 0;
    const random = (size: number) => {
      calls += 1;
      return new Uint8Array(size).fill(calls === 1 ? 0 : 1);
    };
    expect(newTraceId(random)).toMatch(/^[0-9a-f]{32}$/);
    expect(calls).toBe(2);
    calls = 0;
    expect(newSpanId(random)).toMatch(/^[0-9a-f]{16}$/);
    expect(calls).toBe(2);
  });
});
