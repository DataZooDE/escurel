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

describe('resolvePageMessage: open-wikilink', () => {
  it('hands a wikilink to the resolver, and refuses an empty or non-string one', () => {
    expect(
      resolvePageMessage(model, { type: 'open-wikilink', wikilink: '[[supplier::meier-guss]]' }),
    ).toEqual({
      command: 'escurel.resolve',
      args: ['[[supplier::meier-guss]]'],
    });
    expect(resolvePageMessage(model, { type: 'open-wikilink', wikilink: '' })).toBeUndefined();
    expect(
      resolvePageMessage(model, { type: 'open-wikilink', wikilink: 7 as never }),
    ).toBeUndefined();
  });
});

describe('resolvePageMessage: open-original', () => {
  const docModel = {
    ...model,
    preview: { kind: 'document', readOnly: true, chunks: [], total: 0, truncated: false },
  } as unknown as PageModel;

  it('opens the original of THIS page, taken from the host model, only for a document page', () => {
    expect(resolvePageMessage(docModel, { type: 'open-original' })).toEqual({
      command: 'escurel.openOriginal',
      args: [model.pageId],
    });
    expect(resolvePageMessage(model, { type: 'open-original' })).toBeUndefined();
    expect(resolvePageMessage(undefined, { type: 'open-original' })).toBeUndefined();
  });
});

describe('resolvePageMessage: propose-write-back', () => {
  const rowModel = {
    ...model,
    source: {
      sourceFields: ['tier'],
      linked: { enabled: true, exists: false, orphan: false },
      writableColumns: ['tier'],
      etag: 'w1:abc',
    },
  } as unknown as PageModel;

  it('accepts only a column the page itself said is writable, on THIS page', () => {
    expect(resolvePageMessage(rowModel, { type: 'propose-write-back', field: 'tier' })).toEqual({
      command: 'escurel.proposeWriteBack',
      args: [{ pageId: rowModel.pageId, field: 'tier' }],
    });
  });

  it('refuses a column that is not writable, and a page that is not a writable row', () => {
    expect(
      resolvePageMessage(rowModel, { type: 'propose-write-back', field: 'display_name' }),
    ).toBeUndefined();
    expect(
      resolvePageMessage(model, { type: 'propose-write-back', field: 'tier' }),
    ).toBeUndefined();
    expect(
      resolvePageMessage(undefined, { type: 'propose-write-back', field: 'tier' }),
    ).toBeUndefined();
  });
});
