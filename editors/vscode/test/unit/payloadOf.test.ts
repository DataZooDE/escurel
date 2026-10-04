import { describe, expect, it } from 'vitest';
import { payloadOf } from '../../src/client';

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
