import { describe, expect, it } from 'vitest';
import { buildPageModel, fieldView } from '../../src/shared/page';
import type { ExpandResponse, Skill } from '../../src/client';

const skill: Skill = {
  id: 'customer-order',
  description: 'A customer order.',
  summary: 'One order per customer.',
  required_frontmatter: ['status'],
  optional_frontmatter: [],
  is_event_typed: false,
  visibility: 'public',
  owner_field: null,
  backend: { kind: 'sql_view' },
  capabilities: { writable: true, granularity: 'block', search: 'hybrid', supports_crdt: false },
  layer: 'overlay',
  autonomy: 'review',
  actions: [
    { name: 'reassess', kind: 'event', label: 'Reassess risk', event: 'supplier-risk' },
    { name: 'notify', kind: 'event', label: 'Notify customer', event: 'customer-notice' },
    { name: 'ask', kind: 'prompt', label: 'Ask why', prompt: 'why?' },
  ],
  fields: [
    { name: 'status', kind: 'enum', required: true, values: ['open', 'closed'], render: 'badge' },
    { name: 'customer', kind: 'link', required: true, target_skill: 'customer', label: 'Customer' },
    { name: 'value_eur', kind: 'float', required: false, render: 'money' },
    { name: 'eta', kind: 'date', required: false },
    { name: 'urgent', kind: 'bool', required: false },
    { name: 'notes', kind: 'string', required: false, render: 'markdown' },
  ],
};

const expanded: ExpandResponse = {
  page: {
    page_id: 'markdown/instances/customer-order/4500123.md',
    slug: '4500123',
    skill: 'customer-order',
    page_kind: 'instance',
    last_written_by: 'agent:supplier-risk',
  },
  frontmatter: {
    type: 'instance',
    skill: 'customer-order',
    id: '4500123',
    title: 'Customer order 4500123 — Hoffmann Automotive',
    status: 'open',
    customer: '[[customer::hoffmann]]',
    value_eur: 184200,
    eta: '2026-10-02',
    urgent: true,
    summary: 'Delivery at risk.',
  },
  body: '# Order\n\nBody text.',
  blocks: [],
  wikilinks_out: [],
  content_sha256: 'ab'.repeat(32),
};

describe('page model', () => {
  it('folds expand + the skill into fields, summary, body, gate and actions', () => {
    const m = buildPageModel(expanded, skill);
    expect(m.title).toBe('Customer order 4500123 — Hoffmann Automotive');
    expect(m.skill).toMatchObject({
      id: 'customer-order',
      autonomy: 'review',
      readOnly: false,
      backend: 'sql_view',
    });
    expect(m.fields.map((f) => [f.name, f.kind, f.render])).toEqual([
      ['status', 'enum', 'badge'],
      ['customer', 'link', 'link'],
      ['value_eur', 'float', 'money'],
      ['eta', 'date', 'date'],
      ['urgent', 'bool', 'text'],
      ['notes', 'string', 'markdown'],
    ]);
    const link = m.fields.find((f) => f.name === 'customer')!;
    expect(link.links).toEqual([
      { skill: 'customer', id: 'hoffmann', wikilink: '[[customer::hoffmann]]' },
    ]);
    expect(m.fields.find((f) => f.name === 'value_eur')!.display).toBe('184,200.00');
    expect(m.fields.find((f) => f.name === 'notes')!.display).toBe('');
    expect(m.summary).toBe('Delivery at risk.');
    expect(m.body).toBe('# Order\n\nBody text.');
    expect(m.lastWrittenBy).toBe('agent:supplier-risk');
    expect(m.editable).toBe(false);
    expect(m.actions).toEqual([
      { skill: 'supplier-risk', label: 'Reassess risk' },
      { skill: 'customer-notice', label: 'Notify customer' },
    ]);
  });

  it('reads wikilinks out of every shape YAML produces', () => {
    const f = { name: 'primary_owner', kind: 'string', required: false };
    // An unquoted `[[contact::lang]]` in YAML parses to a nested list.
    expect(fieldView(f, [['contact::lang']]).links).toEqual([
      { skill: 'contact', id: 'lang', wikilink: '[[contact::lang]]' },
    ]);
    expect(fieldView(f, '[[contact::lang]]').links?.[0]?.id).toBe('lang');
    expect(fieldView(f, [['contact::lang'], ['contact::brandt']]).links?.map((l) => l.id)).toEqual([
      'lang',
      'brandt',
    ]);
    expect(fieldView(f, 'see [[contact::lang]] first').links?.[0]?.id).toBe('lang');
    expect(fieldView(f, [['contact::lang'], ['contact::brandt']]).display).toBe('lang, brandt');
    // A plain value stays plain; a bare `a::b` outside a list is not a link.
    expect(fieldView(f, 'DE').links).toBeUndefined();
    expect(fieldView(f, 'contact::lang').links).toBeUndefined();
  });

  it('a skill without fields[] shows the frontmatter keys as string fields, minus type/skill/id', () => {
    const { fields, ...bare } = skill;
    void fields;
    const m = buildPageModel(expanded, bare as Skill);
    expect(m.fields.map((f) => f.name)).toEqual([
      'title',
      'status',
      'customer',
      'value_eur',
      'eta',
      'urgent',
      'summary',
    ]);
    expect(m.fields.every((f) => f.kind === 'string')).toBe(true);
  });
});

describe('buildPageModel: the form shows data, not bookkeeping', () => {
  it('hides the page kind, the skill, the id and a backend binding when the skill declares no fields', () => {
    const skill = {
      id: 'order-lines',
      description: 'd',
      backend: { kind: 'sql_view' },
      layer: 'overlay',
      autonomy: 'review',
    } as unknown as Skill;
    const model = buildPageModel(
      {
        page: {
          page_id: 'markdown/instances/order-lines/all.md',
          slug: 'all',
          skill: 'order-lines',
          page_kind: 'instance',
        },
        frontmatter: {
          kind: 'instance',
          skill: 'order-lines',
          id: 'all',
          backend_ref: { kind: 'sql_view', view: 'vw_order_lines__all' },
          customer: 'Hoffmann',
        },
        body: '',
        blocks: [],
        wikilinks_out: [],
      } as unknown as ExpandResponse,
      skill,
    );
    expect(model.fields.map((f) => f.name)).toEqual(['customer']);
  });
});

describe('buildPageModel on a row instance', () => {
  it('says the page is a read-only row with notes, and when it was fetched', () => {
    const e = {
      page: {
        page_id: 'markdown/instances/customer-order/order-4500131.md',
        slug: 'order-4500131',
        skill: 'customer-order',
        page_kind: 'instance',
      },
      frontmatter: { sales_doc: 4500131, delivery_risk: 'low' },
      body: 'Notes',
      blocks: [],
      wikilinks_out: [],
      backend_projection: {
        instances: 'rows',
        read_only: true,
        fetched_at: '2026-10-03T12:03:44.000000Z',
        source: { sales_doc: 4500131 },
        linked: { enabled: true, exists: true, orphan: false },
      },
    } as unknown as Parameters<typeof buildPageModel>[0];
    const skill = {
      id: 'customer-order',
      description: '',
      fields: [],
      backend: { kind: 'sql_view' },
      layer: 'overlay',
      actions: [],
    } as unknown as Parameters<typeof buildPageModel>[1];
    const model = buildPageModel(e, skill);
    expect(model.source).toEqual({
      fetchedAt: '2026-10-03T12:03:44.000000Z',
      sourceFields: ['sales_doc'],
      linked: { enabled: true, exists: true, orphan: false },
    });
  });

  it('has no source for an ordinary page', () => {
    const e = {
      page: { page_id: 'markdown/instances/x/y.md', skill: 'x', page_kind: 'instance' },
      frontmatter: {},
      body: '',
      blocks: [],
      wikilinks_out: [],
    } as unknown as Parameters<typeof buildPageModel>[0];
    const skill = {
      id: 'x',
      description: '',
      fields: [],
      backend: { kind: 'markdown' },
      layer: 'overlay',
      actions: [],
    } as unknown as Parameters<typeof buildPageModel>[1];
    expect(buildPageModel(e, skill).source).toBeUndefined();
  });

  it("carries the skill's OKF provenance as short facts, and says when it has gone stale", () => {
    const now = Date.parse('2026-10-04T12:00:00Z');
    const fresh = buildPageModel(
      expanded,
      { ...skill, verified: '2026-09-30', stale_after: 'P90D' },
      now,
    );
    expect(fresh.skill.facts).toEqual(['verified 2026-09-30', 'stale after P90D']);
    expect(fresh.skill.stale).toBeUndefined();
    const old = buildPageModel(
      expanded,
      { ...skill, verified: '2026-01-01', stale_after: 'P30D' },
      now,
    );
    expect(old.skill.stale).toBe(true);
    expect(old.skill.facts?.[0]).toBe('stale');
    // A skill that declares nothing adds nothing to the model.
    expect(buildPageModel(expanded, skill, now).skill.facts).toBeUndefined();
  });
});
