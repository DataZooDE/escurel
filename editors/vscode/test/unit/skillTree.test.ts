import { describe, expect, it } from 'vitest';
import type { Skill } from '../../src/client';
import {
  ROLE_ICONS,
  buildSkillTree,
  effectiveRole,
  skillAccessibleName,
  type TreeNode,
} from '../../src/views/skillTree';

const base = (id: string, extra: Partial<Skill> = {}): Skill =>
  ({
    id,
    description: `${id} description`,
    required_frontmatter: [],
    optional_frontmatter: [],
    is_event_typed: false,
    visibility: 'public',
    owner_field: null,
    backend: { kind: 'markdown' },
    capabilities: { writable: true, granularity: 'block', search: 'hybrid', supports_crdt: true },
    layer: 'overlay',
    autonomy: 'review',
    ...extra,
  }) as Skill;

const labels = (nodes: TreeNode[]): string[] =>
  nodes.map((n) => (n.kind === 'folder' ? `${n.label}/` : n.label));

describe('effectiveRole', () => {
  it('takes the declared role when it is one of the four', () => {
    expect(effectiveRole(base('a', { role: 'process' } as Partial<Skill>))).toEqual({
      role: 'process',
      inferred: false,
    });
  });

  it('ignores a role a newer server adds and infers instead', () => {
    expect(effectiveRole(base('a', { role: 'agent' } as Partial<Skill>)).inferred).toBe(true);
  });

  it('infers helper for a SQL view or the query skill, process for a harness, report for run params, else record', () => {
    expect(effectiveRole(base('order-lines', { backend: { kind: 'sql_view' } }))).toEqual({
      role: 'helper',
      inferred: true,
    });
    expect(effectiveRole(base('query')).role).toBe('helper');
    expect(effectiveRole(base('supplier-risk', { harness: 'echo' })).role).toBe('process');
    expect(
      effectiveRole(
        base('rev', { params: [{ name: 'w', kind: 'string', required: false }] } as Partial<Skill>),
      ).role,
    ).toBe('report');
    expect(effectiveRole(base('customer-order')).role).toBe('record');
  });
});

describe('buildSkillTree', () => {
  it('puts skills with no folder at the top level, sorted by role then name', () => {
    const tree = buildSkillTree([
      base('zeta', { role: 'record' } as Partial<Skill>),
      base('alpha', { role: 'helper' } as Partial<Skill>),
      base('beta', { role: 'process' } as Partial<Skill>),
      base('apple', { role: 'record' } as Partial<Skill>),
      base('gamma', { role: 'report' } as Partial<Skill>),
    ]);
    expect(labels(tree)).toEqual(['apple', 'zeta', 'beta', 'gamma', 'alpha']);
  });

  it('nests skills in folders, folders first and by name, created for every segment', () => {
    const tree = buildSkillTree([
      base('customer-order', { folder: 'sales/orders', role: 'record' } as Partial<Skill>),
      base('supplier-risk', { folder: 'sales/risk', role: 'process' } as Partial<Skill>),
      base('supplier-risk-analysis', { folder: 'sales/risk', role: 'record' } as Partial<Skill>),
      base('query', { folder: 'plumbing', role: 'helper' } as Partial<Skill>),
      base('standalone'),
    ]);
    expect(labels(tree)).toEqual(['plumbing/', 'sales/', 'standalone']);
    const sales = tree.find((n) => n.kind === 'folder' && n.label === 'sales');
    if (sales?.kind !== 'folder') throw new Error('sales folder');
    expect(sales.path).toBe('sales');
    expect(labels(sales.children)).toEqual(['orders/', 'risk/']);
    const risk = sales.children.find((n) => n.kind === 'folder' && n.label === 'risk');
    if (risk?.kind !== 'folder') throw new Error('risk folder');
    expect(risk.path).toBe('sales/risk');
    // record before process inside a folder.
    expect(labels(risk.children)).toEqual(['supplier-risk-analysis', 'supplier-risk']);
  });

  it('collapses a folder that holds only helpers, and opens the rest', () => {
    const tree = buildSkillTree([
      base('query', { folder: 'plumbing', role: 'helper' } as Partial<Skill>),
      base('order-lines', { folder: 'plumbing/sap', role: 'helper' } as Partial<Skill>),
      base('customer-order', { folder: 'sales', role: 'record' } as Partial<Skill>),
    ]);
    const byLabel = (l: string) => tree.find((n) => n.kind === 'folder' && n.label === l);
    const plumbing = byLabel('plumbing');
    const sales = byLabel('sales');
    expect(plumbing?.kind === 'folder' && plumbing.collapsed).toBe(true);
    expect(sales?.kind === 'folder' && sales.collapsed).toBe(false);
  });

  it('shows a skill whose folder is malformed at the top level instead of dropping it', () => {
    const tree = buildSkillTree([base('odd', { folder: '/a//b' } as Partial<Skill>)]);
    expect(labels(tree)).toEqual(['odd']);
  });
});

describe('presentation', () => {
  it('has a themed codicon per role', () => {
    expect(ROLE_ICONS).toEqual({
      record: 'database',
      process: 'play-circle',
      report: 'graph',
      helper: 'tools',
    });
  });

  it('names a skill by role, id and autonomy for a screen reader, and says when the role is inferred', () => {
    expect(skillAccessibleName(base('customer-order', { role: 'record' } as Partial<Skill>))).toBe(
      'record skill customer-order, autonomy review',
    );
    expect(skillAccessibleName(base('query', { autonomy: undefined }))).toBe(
      'helper skill (inferred) query, autonomy review',
    );
  });
});
