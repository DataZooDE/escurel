import { describe, expect, it } from 'vitest';
import { buildSkillPageModel } from '../../src/shared/skillPage';
import type { Event, Instance, Skill } from '../../src/client/types';

const skill = (over: Partial<Skill> = {}): Skill => ({
  id: 'customer-order',
  description: 'A customer order and what is done with it.',
  required_frontmatter: ['customer'],
  optional_frontmatter: [],
  is_event_typed: false,
  visibility: 'public',
  owner_field: null,
  backend: { kind: 'markdown' },
  capabilities: { writable: true, granularity: 'page', search: 'fts', supports_crdt: false },
  layer: 'overlay',
  fields: [
    { name: 'customer', kind: 'link', required: true, target_skill: 'customer', label: 'Customer' },
    { name: 'status', kind: 'enum', required: false, values: ['open', 'shipped'] },
  ],
  actions: [
    { name: 'check', kind: 'event', label: 'Check credit', event: 'credit-check' },
    { name: 'chat', kind: 'prompt', label: 'Ask in chat', prompt: 'hi' },
  ],
  folder: 'sales/orders',
  role: 'record',
  tags: ['erp', 'sales'],
  ...over,
});

const instance = (n: number): Instance => ({
  page_id: `markdown/instances/customer-order__order-${n}.md`,
  skill: 'customer-order',
  frontmatter: n % 2 ? { title: `Order ${n}` } : {},
  at: `2026-10-0${(n % 9) + 1}T10:00:00Z`,
});

const ev = (id: string, status: string, at: string, over: Partial<Event> = {}): Event => ({
  event_id: id,
  at,
  source: 'workbench',
  mime: null,
  label_skill: 'customer-order',
  instance_page_id: 'markdown/instances/customer-order__order-1.md',
  status,
  title: `event ${id}`,
  body: null,
  provenance: null,
  kind: 'user',
  root_event_id: id,
  run_id: null,
  ...over,
});

describe('buildSkillPageModel', () => {
  it('describes the skill in the author’s words: role, folder, tags, backend, where it lives', () => {
    const m = buildSkillPageModel(skill(), [], [], 0);
    expect(m.id).toBe('customer-order');
    expect(m.title).toBe('Customer order');
    expect(m.description).toMatch(/customer order/);
    expect(m.facts.map((f) => f.label)).toEqual(
      expect.arrayContaining(['Role', 'Folder', 'Tags', 'Backend', 'Autonomy']),
    );
    expect(m.facts.find((f) => f.label === 'Role')?.value).toBe('record');
    expect(m.facts.find((f) => f.label === 'Folder')?.value).toBe('sales/orders');
    expect(m.facts.find((f) => f.label === 'Tags')?.value).toBe('erp, sales');
    expect(m.facts.find((f) => f.label === 'Backend')?.value).toBe('markdown');
  });

  it('uses the OKF title when the skill has one', () => {
    expect(buildSkillPageModel(skill({ title: 'Sales order' }), [], [], 0).title).toBe(
      'Sales order',
    );
  });

  it('lists the fields with required/optional, kind and what a link points to', () => {
    const f = buildSkillPageModel(skill(), [], [], 0).fields;
    expect(f[0]).toMatchObject({ name: 'customer', label: 'Customer', required: true });
    expect(f[0]!.detail).toContain('customer');
    expect(f[1]).toMatchObject({ name: 'status', required: false });
    expect(f[1]!.detail).toContain('open');
  });

  it('falls back to the frontmatter names when no fields are declared', () => {
    const f = buildSkillPageModel(skill({ fields: undefined }), [], [], 0).fields;
    expect(f.map((x) => x.name)).toEqual(['customer']);
    expect(f[0]!.required).toBe(true);
  });

  it('shows only the actions a person can start (event kind), with the author’s label', () => {
    expect(buildSkillPageModel(skill(), [], [], 0).actions).toEqual([
      { skill: 'credit-check', label: 'Check credit' },
    ]);
  });

  it('lists the first 10 instances, titled, and says how many more there are', () => {
    const many = Array.from({ length: 12 }, (_, i) => instance(i + 1));
    const m = buildSkillPageModel(skill(), many, [], 0);
    expect(m.instances.items).toHaveLength(10);
    expect(m.instances.more).toBe(true);
    expect(m.instances.items[0]).toMatchObject({
      pageId: 'markdown/instances/customer-order__order-1.md',
      title: 'Order 1',
    });
    expect(m.instances.items[1]!.title).toBe('order-2');
    expect(buildSkillPageModel(skill(), many.slice(0, 3), [], 0).instances.more).toBe(false);
  });

  it('lists recent runs: newest first, at most 8, each with a word for how it went', () => {
    const events = [
      ev('a', 'processed', '2026-10-01T10:00:00Z'),
      ev('b', 'inbox', '2026-10-03T10:00:00Z'),
      ev('c', 'processed', '2026-10-02T10:00:00Z', { run_id: '01RUN' }),
    ];
    const r = buildSkillPageModel(skill(), [], events, 0).runs;
    expect(r.map((x) => x.rootEventId)).toEqual(['b', 'c', 'a']);
    expect(r[0]!.state).toBe('waiting');
    expect(r[1]!.state).toBe('done');
    expect(r[1]!.runId).toBe('01RUN');
    const lots = Array.from({ length: 20 }, (_, i) =>
      ev(`e${i}`, 'processed', `2026-10-01T10:${String(i).padStart(2, '0')}:00Z`),
    );
    expect(buildSkillPageModel(skill(), [], lots, 0).runs).toHaveLength(8);
  });

  it('marks a base skill read-only and a stale one in words', () => {
    const m = buildSkillPageModel(
      skill({ layer: 'base@pack@1', verified: '2026-01-01', stale_after: 'P30D' }),
      [],
      [],
      Date.parse('2026-10-01T00:00:00Z'),
    );
    expect(m.readOnly).toBe(true);
    expect(m.stale).toBe(true);
    expect(m.provenance.join(' ')).toContain('stale');
  });
});
