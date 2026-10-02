import { describe, expect, it } from 'vitest';
import { parseTerminalArgs } from '../../src/start/terminalSpec';

// `escurel.startInTerminal` is hidden from the palette but still callable by a command URI or
// another extension, and what it is given goes straight to a token mint.
describe('parseTerminalArgs', () => {
  it('accepts a skill and a page, and an optional real root event', () => {
    expect(parseTerminalArgs({ skill: 's', pageId: 'markdown/instances/a__b.md' })).toEqual({
      skill: 's',
      pageId: 'markdown/instances/a__b.md',
    });
    expect(parseTerminalArgs({ skill: 's', pageId: 'p', rootEventId: '01EV' })).toEqual({
      skill: 's',
      pageId: 'p',
      rootEventId: '01EV',
    });
  });

  it('refuses anything that is not strings, or has no skill', () => {
    for (const bad of [
      undefined,
      null,
      'x',
      7,
      [],
      {},
      { skill: '', pageId: 'p' },
      { skill: 1, pageId: 'p' },
      { skill: 's', pageId: 2 },
      { skill: 's', pageId: 'p', rootEventId: 9 },
    ]) {
      expect(parseTerminalArgs(bad)).toBeUndefined();
    }
  });
});
