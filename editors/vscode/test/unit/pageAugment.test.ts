import { describe, expect, it } from 'vitest';
import {
  PageAugmentRegistry,
  loadAugment,
  resolveAugmentAction,
  resolveAugmentSubmit,
} from '../../src/api/augment';
import type { GatewayClient } from '../../src/api/types';

const client: GatewayClient = { callTool: () => Promise.reject(new Error('unused')) };
const ctx = { pageId: 'markdown/instances/acme-thing/a.md', skill: 'acme-thing', frontmatter: { n: 1 }, client };

const form = {
  id: 'note',
  title: 'Add a note',
  submitLabel: 'Propose',
  fields: [
    { name: 'text', label: 'Note', kind: 'textarea' as const, required: true, maxLength: 50 },
    { name: 'kind', label: 'Kind', kind: 'enum' as const, values: ['a', 'b'] },
    { name: 'flag', label: 'Flag', kind: 'bool' as const },
  ],
  toDraft: (v: Record<string, unknown>) => ({
    target_page_id: 'markdown/instances/acme-thing/a.md',
    content: `note: ${String(v.text)}`,
  }),
};

describe('PageAugmentRegistry', () => {
  it('keeps several augments per skill and removes one on dispose', () => {
    const reg = new PageAugmentRegistry();
    const a = reg.register('acme-thing', { sections: async () => [] });
    reg.register('acme-thing', { actions: () => [] });
    expect(reg.size).toBe(2);
    expect(reg.has('acme-thing')).toBe(true);
    expect(reg.has('other')).toBe(false);
    a.dispose();
    expect(reg.size).toBe(1);
  });

  it('rejects an invalid skill id, an empty augment and a malformed form', () => {
    const reg = new PageAugmentRegistry();
    expect(() => reg.register('bad id', { sections: () => [] })).toThrow(/skill/);
    expect(() => reg.register('acme-thing', {})).toThrow(/at least one/);
    expect(() =>
      reg.register('acme-thing', { forms: [{ ...form, toDraft: undefined as never }] }),
    ).toThrow(/toDraft/);
    expect(() => reg.register('acme-thing', { forms: [{ ...form, id: 'Bad Id' }] })).toThrow(/form id/);
  });
});

describe('loadAugment', () => {
  it('returns sections as cleaned data, actions without commands, forms without toDraft', async () => {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', {
      sections: async () => [
        {
          id: 'impact',
          title: 'Impact‮',
          blocks: [
            { type: 'text', text: 'hello' },
            { type: 'facts', rows: [{ label: 'a', value: 'b', tone: 'attention' }] },
            { type: 'table', columns: ['x', 'y'], rows: [['1', '2']] },
            { type: 'list', items: [{ label: 'open', command: 'acme.example.open', args: [7] }] },
          ],
        },
      ],
      actions: () => [{ id: 'go', label: 'Go', command: 'acme.example.go', args: ['p'] }],
      forms: [form],
    });
    const out = (await loadAugment(reg, 'acme-thing', ctx))!;
    expect(out.view.sections[0]!.title).toBe('Impact');
    expect(out.view.sections[0]!.blocks[3]).toMatchObject({
      type: 'list',
      items: [{ label: 'open', key: expect.any(String) }],
    });
    const wire = JSON.stringify(out.view);
    expect(wire).not.toContain('acme.example');
    expect(wire).not.toContain('toDraft');
    expect(out.view.actions).toEqual([{ id: expect.any(String), label: 'Go' }]);
    expect(out.view.forms[0]).toMatchObject({ title: 'Add a note', submitLabel: 'Propose' });
    const actionId = out.view.actions[0]!.id;
    expect(resolveAugmentAction(out.table, actionId)).toEqual({
      command: 'acme.example.go',
      args: ['p'],
    });
    const listKey = (out.view.sections[0]!.blocks[3] as { items: { key: string }[] }).items[0]!.key;
    expect(resolveAugmentAction(out.table, listKey)).toEqual({
      command: 'acme.example.open',
      args: [7],
    });
  });

  it('refuses an unknown action id and a command with a bad id', async () => {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', {
      actions: () => [
        { id: 'bad', label: 'Bad', command: 'rm -rf /' },
        { id: 'ok', label: 'Ok', command: 'acme.example.ok' },
      ],
    });
    const out = (await loadAugment(reg, 'acme-thing', ctx))!;
    expect(out.view.actions.map((a) => a.label)).toEqual(['Ok']);
    expect(resolveAugmentAction(out.table, 'nope')).toBeUndefined();
  });

  it('a provider that throws or times out costs only its own contribution', async () => {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', {
      sections: async () => {
        throw new Error('boom');
      },
    });
    reg.register('acme-thing', { sections: () => new Promise(() => undefined) });
    reg.register('acme-thing', {
      sections: async () => [{ id: 's', title: 'Fine', blocks: [{ type: 'text', text: 't' }] }],
    });
    const out = (await loadAugment(reg, 'acme-thing', ctx, 20))!;
    expect(out.view.sections.map((s) => s.title)).toEqual(['Fine']);
    expect(out.view.unavailable).toBe(true);
  });

  it('answers undefined when nothing is registered for the skill, and passes the context', async () => {
    const reg = new PageAugmentRegistry();
    expect(await loadAugment(reg, 'acme-thing', ctx)).toBeUndefined();
    let seen: unknown;
    reg.register('acme-thing', {
      sections: async (c) => {
        seen = { pageId: c.pageId, skill: c.skill, fm: c.frontmatter, hasClient: !!c.client };
        return [];
      },
    });
    await loadAugment(reg, 'acme-thing', ctx);
    expect(seen).toEqual({ pageId: ctx.pageId, skill: 'acme-thing', fm: { n: 1 }, hasClient: true });
  });

  it('caps sections, blocks, rows and text', async () => {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', {
      sections: async () =>
        Array.from({ length: 40 }, (_, i) => ({
          id: `s${i}`,
          title: 't',
          blocks: [{ type: 'table' as const, columns: ['c'], rows: Array.from({ length: 500 }, () => ['x'.repeat(5000)]) }],
        })),
    });
    const out = (await loadAugment(reg, 'acme-thing', ctx))!;
    expect(out.view.sections.length).toBeLessThanOrEqual(12);
    const t = out.view.sections[0]!.blocks[0] as { rows: string[][] };
    expect(t.rows.length).toBeLessThanOrEqual(50);
    expect(t.rows[0]![0]!.length).toBeLessThanOrEqual(400);
  });
});

describe('resolveAugmentSubmit', () => {
  async function table() {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', { forms: [form] });
    const out = (await loadAugment(reg, 'acme-thing', ctx))!;
    return { out, id: out.view.forms[0]!.id };
  }

  it('validates the values again and returns only create_draft arguments', async () => {
    const { out, id } = await table();
    const r = await resolveAugmentSubmit(out.table, id, { text: 'hi', kind: 'a', extra: 'dropped' }, ctx);
    expect(r).toEqual({
      ok: true,
      args: { target_page_id: 'markdown/instances/acme-thing/a.md', content: 'note: hi' },
    });
  });

  it('refuses missing required, non-member enum value, too long text and unknown form', async () => {
    const { out, id } = await table();
    expect((await resolveAugmentSubmit(out.table, id, { text: '  ' }, ctx)).ok).toBe(false);
    expect((await resolveAugmentSubmit(out.table, id, { text: 'x', kind: 'z' }, ctx)).ok).toBe(false);
    expect((await resolveAugmentSubmit(out.table, id, { text: 'x'.repeat(51) }, ctx)).ok).toBe(false);
    expect((await resolveAugmentSubmit(out.table, id, 'nope' as never, ctx)).ok).toBe(false);
    expect((await resolveAugmentSubmit(out.table, 'unknown', { text: 'x' }, ctx)).ok).toBe(false);
  });

  it('refuses what toDraft returns when it is more than a draft of a page, and when it throws', async () => {
    const reg = new PageAugmentRegistry();
    reg.register('acme-thing', {
      forms: [
        { ...form, id: 'bad1', toDraft: () => ({ target_page_id: '../x', content: 'c' }) },
        { ...form, id: 'bad2', toDraft: () => ({ target_page_id: 'markdown/a.md', content: 5 as never }) },
        { ...form, id: 'bad3', toDraft: () => { throw new Error('x'); } },
        {
          ...form,
          id: 'extra',
          toDraft: () =>
            ({ target_page_id: 'markdown/a.md', content: 'c', changeset_id: 'zzz', base_sha256: 'a'.repeat(64) }) as never,
        },
      ],
    });
    const out = (await loadAugment(reg, 'acme-thing', ctx))!;
    const ids = out.view.forms.map((f) => f.id);
    for (const id of ids.slice(0, 3)) {
      expect((await resolveAugmentSubmit(out.table, id, { text: 'x' }, ctx)).ok).toBe(false);
    }
    // only the three create_draft keys the form may set survive
    expect(await resolveAugmentSubmit(out.table, ids[3]!, { text: 'x' }, ctx)).toEqual({
      ok: true,
      args: { target_page_id: 'markdown/a.md', content: 'c', base_sha256: 'a'.repeat(64) },
    });
  });
});
