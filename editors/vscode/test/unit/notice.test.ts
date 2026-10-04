import { describe, expect, it } from 'vitest';
import { noticeActions } from '../../src/shared/notice';

// A notification that names a page, a run or a thread offers to open it: the person should never have
// to hunt for the thing the message is about. One helper builds the buttons, so every notice speaks the
// same way and keeps to two buttons (a toast stays small).
describe('noticeActions', () => {
  it('turns each target into one button that runs the right command', () => {
    expect(
      noticeActions([
        { kind: 'page', pageId: 'markdown/instances/customer-order__order-4500131.md' },
        { kind: 'run', runId: '01RUN' },
      ]),
    ).toEqual([
      {
        label: 'Open order-4500131',
        command: 'escurel.openInstance',
        args: ['markdown/instances/customer-order__order-4500131.md'],
      },
      { label: 'Open run', command: 'escurel.openRun', args: ['01RUN'] },
    ]);
    expect(noticeActions([{ kind: 'thread', rootEventId: '01EV' }])[0]).toEqual({
      label: 'Open thread',
      command: 'escurel.openThread',
      args: ['01EV'],
    });
    expect(noticeActions([{ kind: 'skill', skill: 'supplier-risk' }])[0]).toEqual({
      label: 'View skill',
      command: 'escurel.viewSkill',
      args: ['supplier-risk'],
    });
  });

  it('keeps at most two buttons, in the order given', () => {
    const a = noticeActions([
      { kind: 'page', pageId: 'markdown/instances/s__a.md' },
      { kind: 'page', pageId: 'markdown/instances/s__b.md' },
      { kind: 'page', pageId: 'markdown/instances/s__c.md' },
      { kind: 'thread', rootEventId: '01EV' },
    ]);
    expect(a.map((x) => x.label)).toEqual(['Open a', 'Open b']);
  });

  it('drops duplicates and empty targets', () => {
    const a = noticeActions([
      { kind: 'run', runId: '01RUN' },
      { kind: 'run', runId: '01RUN' },
      { kind: 'page', pageId: '' },
      { kind: 'thread', rootEventId: undefined },
    ]);
    expect(a.map((x) => x.label)).toEqual(['Open run']);
  });

  it('is empty when there is nothing to open', () => {
    expect(noticeActions([])).toEqual([]);
  });
});
