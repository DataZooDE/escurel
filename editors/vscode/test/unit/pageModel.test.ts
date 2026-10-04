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
  it('shows validation only on the exact ready experiment projection', () => {
    const validationSkill = { ...skill, id: 'evolve_experiment', actions: [
      { name: 'validate-winner', kind: 'event' as const, label: 'Validate winner', event: 'evolve_validate' },
    ] };
    const ready = { ...expanded, frontmatter: { ...expanded.frontmatter,
      status: 'completed', best_program_id: 7,
      next_validation_action: 'evolve_validate_winner',
    } };
    expect(buildPageModel(ready, validationSkill).actions).toHaveLength(1);
    expect(buildPageModel({ ...ready, frontmatter: { ...ready.frontmatter,
      next_validation_action: null,
    } }, validationSkill).actions).toHaveLength(0);
    expect(buildPageModel({ ...ready, content_sha256: undefined }, validationSkill).actions).toHaveLength(0);
  });
  it('offers candidate publication only on a passed private report with exact bindings', () => {
    const reportSkill = { ...skill, id: 'evolve_validation_report', actions: [
      { name: 'create-policy-candidate', kind: 'event' as const, label: 'Create policy candidate', event: 'evolve_publish_candidate' },
    ] };
    const ready = { ...expanded, frontmatter: { ...expanded.frontmatter,
      status: 'passed', effective_passed: true, winner_program_id: 7,
      report_sha256: 'a'.repeat(64), next_candidate_action: 'evolve_publish_candidate',
    } };
    expect(buildPageModel(ready, reportSkill).actions).toHaveLength(1);
    expect(buildPageModel({ ...ready, frontmatter: { ...ready.frontmatter,
      effective_passed: false,
    } }, reportSkill).actions).toHaveLength(0);
    expect(buildPageModel({ ...ready, frontmatter: { ...ready.frontmatter,
      status: 'conflicted',
    } }, reportSkill).actions).toHaveLength(0);
  });
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
