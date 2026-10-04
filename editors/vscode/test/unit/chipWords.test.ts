import { describe, expect, it } from 'vitest';
import { chipWords } from '../../src/thread/chipWords';

describe('chipWords', () => {
  it('gives every state a short word and a shape, never the wire word', () => {
    expect(chipWords('processed')).toEqual({ text: 'done', icon: 'check' });
    expect(chipWords('inbox')).toEqual({ text: 'waiting', icon: 'clock' });
    expect(chipWords('dead_letter')).toEqual({ text: 'gave up', icon: 'cross' });
    expect(chipWords('promoted')).toEqual({ text: 'applied', icon: 'check' });
    expect(chipWords('running').icon).toBe('sync');
  });
  it('shows an unknown state as sent, without an icon', () => {
    expect(chipWords('on_hold')).toEqual({ text: 'on hold', icon: 'none' });
  });
});
