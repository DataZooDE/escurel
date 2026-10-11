import { describe, expect, it } from 'vitest';
import { FocusSuggestion, shouldSuggestFocus, SUGGESTED_KEY } from '../../src/focus/focusSuggestion';

const eligible = { connected: true, focusActive: false, alreadySuggested: false, silentBootstrap: false };

describe('first-connect focus suggestion', () => {
  it('requires a connection, classic view, no previous suggestion and no silent bootstrap', () => {
    for (const connected of [false, true]) for (const focusActive of [false, true])
      for (const alreadySuggested of [false, true]) for (const silentBootstrap of [false, true]) {
        expect(shouldSuggestFocus({ connected, focusActive, alreadySuggested, silentBootstrap }))
          .toBe(connected && !focusActive && !alreadySuggested && !silentBootstrap);
      }
  });

  function setup(choice?: string) {
    const saved = new Map<string, boolean>();
    const notices: string[][] = [];
    const commands: string[] = [];
    let state = { ...eligible };
    const suggestion = new FocusSuggestion({
      state: () => ({ ...state, alreadySuggested: saved.get(SUGGESTED_KEY) ?? false }),
      remember: async () => { saved.set(SUGGESTED_KEY, true); },
      show: async (...args: string[]) => { notices.push(args); return choice; },
      enter: async () => { commands.push('escurel.focusMode.enter'); },
    });
    return { suggestion, saved, notices, commands, setState: (next: typeof state) => { state = next; } };
  }

  for (const choice of [undefined, 'Not now', "Don't ask again", 'Switch to focus view']) {
    it(`persists before showing and never repeats after ${choice ?? 'dismissal'}`, async () => {
      const test = setup(choice);
      await Promise.all([test.suggestion.check(), test.suggestion.check()]);
      await test.suggestion.check();
      expect(test.saved.get(SUGGESTED_KEY)).toBe(true);
      expect(test.notices).toEqual([[
        'Switch to the calm focus view? You can switch back at any time.',
        'Switch to focus view', 'Not now', "Don't ask again",
      ]]);
      expect(test.commands).toEqual(choice === 'Switch to focus view' ? ['escurel.focusMode.enter'] : []);
    });
  }

  it('does not consume the suggestion while disconnected or suppressed', async () => {
    const test = setup();
    test.setState({ ...eligible, connected: false });
    await test.suggestion.check();
    test.setState({ ...eligible, silentBootstrap: true });
    await test.suggestion.check();
    expect(test.saved.size).toBe(0);
    expect(test.notices).toEqual([]);
    test.setState(eligible);
    await test.suggestion.check();
    expect(test.notices).toHaveLength(1);
  });
});
