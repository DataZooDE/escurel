import { describe, expect, it } from 'vitest';
import {
  ReviewHookRegistry,
  collectBefore,
  collectPreview,
  runAfter,
} from '../../src/review/hooks';
import type { GatewayClient } from '../../src/api/types';

const client: GatewayClient = { callTool: () => Promise.reject(new Error('unused')) };
const drafts = [{ draftId: 'd1', targetPageId: 'markdown/instances/acme-thing/a.md' }];

describe('ReviewHookRegistry', () => {
  it('keeps hooks in registration order and removes one on dispose', () => {
    const reg = new ReviewHookRegistry();
    const a = reg.register({ before: () => undefined });
    reg.register({ after: () => undefined });
    expect(reg.size).toBe(2);
    a.dispose();
    expect(reg.size).toBe(1);
  });

  it('rejects an empty hook and a member that is not a function', () => {
    const reg = new ReviewHookRegistry();
    expect(() => reg.register({})).toThrow(/at least one/);
    expect(() => reg.register({ before: 'x' as never })).toThrow(/function/);
    expect(() => reg.register(null as never)).toThrow();
  });

  it('knows whether any hook wants a preview', () => {
    const reg = new ReviewHookRegistry();
    reg.register({ before: () => undefined });
    expect(reg.hasPreview).toBe(false);
    reg.register({ preview: () => [] });
    expect(reg.hasPreview).toBe(true);
  });
});

describe('collectBefore', () => {
  const ctx = { decision: 'accepted' as const, drafts, client };

  it('asks for nothing when no hook says anything', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({ before: () => undefined });
    expect(await collectBefore(reg, ctx)).toEqual({ note: 'none' });
  });

  it('the strictest note wins: required over optional over none, and a prompt is kept', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({ before: () => ({ note: 'optional', notePrompt: 'Why?' }) });
    reg.register({ before: async () => ({ note: 'required', notePrompt: 'Reason‮ for the change' }) });
    reg.register({ before: () => ({ note: 'optional' }) });
    expect(await collectBefore(reg, ctx)).toEqual({ note: 'required', prompt: 'Reason for the change' });
  });

  it('passes decision, drafts and a client to the hook', async () => {
    const reg = new ReviewHookRegistry();
    let seen: unknown;
    reg.register({
      before: (c) => {
        seen = { d: c.decision, drafts: c.drafts, hasClient: !!c.client };
      },
    });
    await collectBefore(reg, { decision: 'rejected', drafts, client });
    expect(seen).toEqual({ d: 'rejected', drafts, hasClient: true });
  });

  it('a veto blocks, with the cleaned message of the first veto', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({ before: () => ({ veto: 'Not yet‮.' }) });
    reg.register({ before: () => ({ veto: 'second' }) });
    expect(await collectBefore(reg, ctx)).toMatchObject({ veto: 'Not yet.' });
  });

  it('an error, a timeout or nonsense from a hook never blocks', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({
      before: () => {
        throw new Error('boom');
      },
    });
    reg.register({ before: () => new Promise(() => undefined) });
    reg.register({ before: () => ({ veto: '' }) });
    reg.register({ before: () => ({ veto: 5 as never, note: 'bogus' as never }) });
    reg.register({ before: () => 'x' as never });
    expect(await collectBefore(reg, ctx, 20)).toEqual({ note: 'none' });
  });

  it('a hook that throws does not hide a veto of another', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({
      before: () => {
        throw new Error('boom');
      },
    });
    reg.register({ before: () => ({ veto: 'No.' }) });
    expect(await collectBefore(reg, ctx)).toMatchObject({ veto: 'No.' });
  });
});

describe('runAfter', () => {
  const rec = {
    draftId: 'd1',
    targetPageId: 'markdown/instances/acme-thing/a.md',
    decision: 'accepted' as const,
    note: 'ok',
    provenance: { author: 'agent:x', run_id: 'r1' },
    client,
  };

  it('gives every hook the recorded decision, and one failing hook does not stop the others', async () => {
    const reg = new ReviewHookRegistry();
    const seen: unknown[] = [];
    reg.register({
      after: () => {
        throw new Error('boom');
      },
    });
    reg.register({ after: (c) => void seen.push({ ...c, client: undefined }) });
    reg.register({ after: () => Promise.reject(new Error('async boom')) });
    await runAfter(reg, rec);
    expect(seen).toEqual([{ ...rec, client: undefined }]);
  });

  it('does not wait forever for a hook', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({ after: () => new Promise(() => undefined) });
    await runAfter(reg, rec, 20);
  });

  it('hands out only string provenance', async () => {
    const reg = new ReviewHookRegistry();
    let got: unknown;
    reg.register({ after: (c) => void (got = c.provenance) });
    await runAfter(reg, { ...rec, provenance: { a: 'x', b: 5 as never, c: null as never } });
    expect(got).toEqual({ a: 'x' });
  });
});

describe('collectPreview', () => {
  const pctx = { draftId: 'd1', targetPageId: 'markdown/a.md', client };

  it('turns the sections of every hook into one cleaned view with list keys', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({
      preview: async () => [
        {
          id: 'impact',
          title: 'Impact‮',
          blocks: [{ type: 'list', items: [{ label: 'go', command: 'acme.example.go', args: [1] }] }],
        },
      ],
    });
    reg.register({
      preview: () => {
        throw new Error('boom');
      },
    });
    reg.register({ before: () => undefined });
    const out = (await collectPreview(reg, pctx))!;
    expect(out.view.sections.map((s) => s.title)).toEqual(['Impact']);
    expect(out.view.unavailable).toBe(true);
    const key = (out.view.sections[0]!.blocks[0] as { items: { key: string }[] }).items[0]!.key;
    expect(out.commands.get(key)).toEqual({ command: 'acme.example.go', args: [1] });
    expect(JSON.stringify(out.view)).not.toContain('acme.example.go');
  });

  it('answers undefined when no hook adds a section', async () => {
    const reg = new ReviewHookRegistry();
    reg.register({ preview: () => [] });
    expect(await collectPreview(reg, pctx)).toBeUndefined();
    expect(await collectPreview(new ReviewHookRegistry(), pctx)).toBeUndefined();
  });
});
