import { describe, expect, it } from 'vitest';
import { buildPageModel, actionLabel } from '../../src/shared/page';
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
  actions: ['supplier-risk', 'customer-notice'],
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
    page_type: 'instance',
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
    expect(link.link).toEqual({
      skill: 'customer',
      id: 'hoffmann',
      pageId: 'markdown/instances/customer/hoffmann.md',
    });
    expect(m.fields.find((f) => f.name === 'value_eur')!.display).toBe('184,200.00');
    expect(m.fields.find((f) => f.name === 'notes')!.display).toBe('');
    expect(m.summary).toBe('Delivery at risk.');
    expect(m.body).toBe('# Order\n\nBody text.');
    expect(m.lastWrittenBy).toBe('agent:supplier-risk');
    expect(m.editable).toBe(false);
    expect(m.actions.map((a) => a.skill)).toEqual(['supplier-risk', 'customer-notice']);
  });

  it('derives the action label until PR-2: "<Skill title> for <instance title> with an agent"', () => {
    expect(actionLabel('supplier-risk', '4500123')).toBe('Supplier risk for 4500123 with an agent');
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
