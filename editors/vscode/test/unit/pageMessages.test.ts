import { describe, expect, it } from 'vitest';
import { resolvePageMessage } from '../../src/editors/pageMessages';
import type { PageModel } from '../../src/shared/protocol';

const model = {
  pageId: 'markdown/instances/customer-order__order-4500123.md',
  skill: { id: 'customer-order' },
  actions: [{ skill: 'supplier-risk', label: 'Reassess risk' }],
} as unknown as PageModel;

// The page host is the one M4 surface whose webview messages were not checked. A webview is not
// trusted: it can say anything, so the host decides what it may start from the model IT built.
describe('resolvePageMessage: start-skill', () => {
  it('starts a skill the page offers, on THIS page', () => {
    expect(
      resolvePageMessage(model, {
        type: 'start-skill',
        skill: 'supplier-risk',
        mode: 'background',
      }),
    ).toEqual({
      command: 'escurel.startSkill',
      args: [{ skill: 'supplier-risk', pageId: model.pageId, mode: 'background' }],
    });
  });

  it('refuses a skill the page does not offer', () => {
    expect(
      resolvePageMessage(model, {
        type: 'start-skill',
        skill: 'delete-everything',
        mode: 'background',
      }),
    ).toBeUndefined();
  });

  it('refuses a mode that is not one of the three', () => {
    expect(
      resolvePageMessage(model, {
        type: 'start-skill',
        skill: 'supplier-risk',
        mode: 'sudo' as never,
      }),
    ).toBeUndefined();
  });

  it('does nothing before a page has been loaded', () => {
    expect(
      resolvePageMessage(undefined, { type: 'start-skill', skill: 'supplier-risk', mode: 'plan' }),
    ).toBeUndefined();
  });
});

describe('resolvePageMessage: view-skill and the read-only opens', () => {
  it('shows the page’s own skill or one of its actions, nothing else', () => {
    expect(resolvePageMessage(model, { type: 'view-skill', skill: 'customer-order' })).toEqual({
      command: 'escurel.viewSkill',
      args: ['customer-order'],
    });
    expect(resolvePageMessage(model, { type: 'view-skill', skill: 'supplier-risk' })?.command).toBe(
      'escurel.viewSkill',
    );
    expect(resolvePageMessage(model, { type: 'view-skill', skill: 'other' })).toBeUndefined();
  });

  it('passes an id to a read-only open only when it is a non-empty string', () => {
    expect(resolvePageMessage(model, { type: 'open-run', runId: '01RUN' })).toEqual({
      command: 'escurel.openRun',
      args: ['01RUN'],
    });
    expect(resolvePageMessage(model, { type: 'open-run', runId: 42 as never })).toBeUndefined();
    expect(resolvePageMessage(model, { type: 'open-thread', rootEventId: '' })).toBeUndefined();
  });
});
