import { describe, expect, it } from 'vitest';
import { chooseHarness } from '../../src/start/chooseHarness';

const MULTI = { harness: 'echo', harness_allow: ['claude', 'echo', 'gemini'] };

describe('chooseHarness', () => {
  it('sends nothing when the setting is blank and no status is known', () => {
    expect(chooseHarness('', null)).toEqual({ kind: 'none' });
    expect(chooseHarness('  ', undefined)).toEqual({ kind: 'none' });
  });

  it('keeps today’s behaviour on an old runner (no allow-list): blank sends none, set sends', () => {
    expect(chooseHarness('', { harness: 'echo' })).toEqual({ kind: 'none' });
    expect(chooseHarness('claude', { harness: 'echo' })).toEqual({
      kind: 'send',
      harness: 'claude',
    });
  });

  it('sends nothing to pick when the list has one entry', () => {
    expect(chooseHarness('', { harness: 'echo', harness_allow: ['echo'] })).toEqual({
      kind: 'none',
    });
  });

  it('asks with the default preselected when the setting is blank and the list has several', () => {
    expect(chooseHarness('', MULTI)).toEqual({
      kind: 'ask',
      harnesses: ['claude', 'echo', 'gemini'],
      defaultHarness: 'echo',
    });
  });

  it('falls back to the first entry when the default is outside the list', () => {
    expect(chooseHarness('', { harness: 'muse', harness_allow: ['claude', 'echo'] })).toEqual({
      kind: 'ask',
      harnesses: ['claude', 'echo'],
      defaultHarness: 'claude',
    });
  });

  it('sends a configured harness that is on the published list', () => {
    expect(chooseHarness('gemini', MULTI)).toEqual({ kind: 'send', harness: 'gemini' });
  });

  it('refuses, visibly, a configured harness outside the published list', () => {
    expect(chooseHarness('muse', MULTI)).toEqual({
      kind: 'refused',
      configured: 'muse',
      allowed: ['claude', 'echo', 'gemini'],
    });
  });

  it('never sends the unlisted name: a refusal carries no harness to send', () => {
    const choice = chooseHarness('muse', MULTI);
    expect(choice.kind).toBe('refused');
    expect(choice).not.toHaveProperty('harness');
  });

  it('caps untrusted status text: 20 names, 40 chars, cleaned', () => {
    const long = 'x'.repeat(41);
    const allow = [
      '  claude  ',
      'bad\u202ename',
      long,
      ...Array.from({ length: 25 }, (_, i) => `h${i}`),
    ];
    const choice = chooseHarness('', { harness: 'echo', harness_allow: allow });
    expect(choice.kind).toBe('ask');
    if (choice.kind !== 'ask') return;
    expect(choice.harnesses.length).toBeLessThanOrEqual(20);
    expect(choice.harnesses).not.toContain(long);
    expect(choice.harnesses).toContain('claude');
    for (const name of choice.harnesses) {
      expect(name.length).toBeLessThanOrEqual(40);
      // eslint-disable-next-line no-control-regex
      expect(name).not.toMatch(/[\u0000-\u001f\u202a-\u202e]/);
    }
  });
});
