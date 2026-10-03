import { describe, expect, it } from 'vitest';
import { skillActionViews } from '../../src/shared/actions';

// A skill declares its follow-ups as objects (Peacock's form): the label is the skill author's own
// words, and only an `event` action can be started from here. Until now the extension derived
// "Supplier risk for 4500123 with an agent" from a bare skill id.
describe('skillActionViews', () => {
  it('turns kind=event actions into buttons: the author’s label, the skill the event is filed under', () => {
    expect(
      skillActionViews([
        {
          name: 'notify-customer',
          kind: 'event',
          label: 'Notify customer',
          event: 'customer-notice',
        },
        { name: 'rerun', kind: 'event', label: 'Re-run analysis', event: 'supplier-risk' },
      ]),
    ).toEqual([
      { skill: 'customer-notice', label: 'Notify customer' },
      { skill: 'supplier-risk', label: 'Re-run analysis' },
    ]);
  });

  it('does not offer a prompt action: it is a chat turn, not something the workbench starts', () => {
    expect(
      skillActionViews([
        { name: 'ask-why', kind: 'prompt', label: 'Ask why', prompt: 'why is {id} at risk?' },
        { name: 'rerun', kind: 'event', label: 'Re-run', event: 'supplier-risk' },
      ]).map((a) => a.skill),
    ).toEqual(['supplier-risk']);
  });

  it('skips an event action that names no skill, and tolerates none declared', () => {
    expect(skillActionViews([{ name: 'x', kind: 'event', label: 'X' }])).toEqual([]);
    expect(skillActionViews(undefined)).toEqual([]);
  });

  it('never invents a label from the skill id', () => {
    expect(
      skillActionViews([{ name: 'x', kind: 'event', label: '', event: 'supplier-risk' }]),
    ).toEqual([]);
  });
});
