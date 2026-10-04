import { describe, expect, it } from 'vitest';
import { payloadOf, refusalOf } from '../../src/client';

describe('payloadOf', () => {
  it('prefers structuredContent over the summary text', () => {
    expect(
      payloadOf({
        content: [{ type: 'text', text: '3 events. Full result in structuredContent.' }],
        structuredContent: { events: [1, 2, 3] },
      }),
    ).toEqual({ events: [1, 2, 3] });
  });

  it('still decodes a legacy gateway that sent the payload as JSON text only', () => {
    expect(payloadOf({ content: [{ type: 'text', text: '{"events":[1]}' }] })).toEqual({
      events: [1],
    });
  });

  it('does not mistake a summary for a payload', () => {
    expect(payloadOf({ content: [{ type: 'text', text: '3 events' }] })).toEqual({});
  });
});

// A refused read must never look like data. The extension already throws on `isError`; what it lost
// was the tool's own words when the refusal carried no `issues`.
describe('refusalOf', () => {
  it('is undefined for a success', () => {
    expect(
      refusalOf({ content: [{ type: 'text', text: '1 row' }], structuredContent: { rows: [1] } }),
    ).toBeUndefined();
  });

  it('returns the payload of an isError result with its issues', () => {
    const issues = [{ severity: 'error', code: 'invalid_limit', location: '', message: 'bad' }];
    expect(refusalOf({ isError: true, structuredContent: { ok: false, issues } })?.issues).toEqual(
      issues,
    );
  });

  it('treats ok:false without the flag as a refusal too', () => {
    expect(refusalOf({ structuredContent: { ok: false, issues: [{ code: 'x' }] } })).toBeDefined();
  });

  it('keeps the tool’s own text when a refusal names no issue', () => {
    const r = refusalOf({ isError: true, content: [{ type: 'text', text: 'the source is down' }] });
    expect((r?.issues as { code: string; message: string }[])[0]).toMatchObject({
      code: 'tool_error',
      message: 'the source is down',
    });
  });
});
