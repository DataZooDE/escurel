import { describe, expect, it } from 'vitest';
import type { Skill } from '../../src/client';
import {
  buildSkillTree,
  describeFilter,
  filterSkills,
  knownTags,
  type TreeNode,
} from '../../src/views/skillTree';

const sk = (id: string, extra: Partial<Skill> = {}): Skill =>
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

const SKILLS = [
  sk('customer-order', { folder: 'sales/orders', tags: ['sap', 'sales'], title: 'Customer order' }),
  sk('supplier', { folder: 'purchasing/suppliers', tags: ['sap', 'purchasing'] }),
  sk('supplier-risk', {
    folder: 'purchasing/risk',
    tags: ['purchasing', 'risk/analysis'],
    summary: 'Assess a vendor signal',
  }),
  sk('query', { folder: 'plumbing', tags: [] }),
  sk('loose'),
];

const ids = (skills: Skill[]) => skills.map((s) => s.id);

describe('filterSkills', () => {
  it('without a filter every skill stays', () => {
    expect(filterSkills(SKILLS, {})).toBe(SKILLS);
    expect(filterSkills(SKILLS, { tag: '  ', text: '' })).toBe(SKILLS);
  });

  it('by tag: exact, case-insensitive', () => {
    expect(ids(filterSkills(SKILLS, { tag: 'sap' }))).toEqual(['customer-order', 'supplier']);
    expect(ids(filterSkills(SKILLS, { tag: 'SAP' }))).toEqual(['customer-order', 'supplier']);
    expect(ids(filterSkills(SKILLS, { tag: 'sa' }))).toEqual([]);
  });

  it('a tag path matches its parent: filtering by `risk` finds `risk/analysis`', () => {
    expect(ids(filterSkills(SKILLS, { tag: 'risk' }))).toEqual(['supplier-risk']);
    expect(ids(filterSkills(SKILLS, { tag: 'risk/analysis' }))).toEqual(['supplier-risk']);
  });

  it('by text: id, title, summary, description, folder and tags', () => {
    expect(ids(filterSkills(SKILLS, { text: 'vendor' }))).toEqual(['supplier-risk']);
    expect(ids(filterSkills(SKILLS, { text: 'Customer order' }))).toEqual(['customer-order']);
    expect(ids(filterSkills(SKILLS, { text: 'plumbing' }))).toEqual(['query']);
    expect(ids(filterSkills(SKILLS, { text: 'PURCHASING' }))).toEqual([
      'supplier',
      'supplier-risk',
    ]);
  });

  it('tag and text together both have to match', () => {
    expect(ids(filterSkills(SKILLS, { tag: 'sap', text: 'supplier' }))).toEqual(['supplier']);
  });
});

describe('a filtered tree keeps only the folders that still hold something', () => {
  const labels = (nodes: TreeNode[]): string[] =>
    nodes.map((n) => (n.kind === 'folder' ? `${n.label}/` : n.label));

  it('drops empty folders and keeps the path to a match', () => {
    const tree = buildSkillTree(filterSkills(SKILLS, { tag: 'purchasing' }));
    expect(labels(tree)).toEqual(['purchasing/']);
    const folder = tree[0]!;
    expect(folder.kind === 'folder' && labels(folder.children)).toEqual(['risk/', 'suppliers/']);
  });
});

describe('knownTags', () => {
  it('lists each tag once with how many skills carry it, most used first then by name', () => {
    expect(knownTags(SKILLS)).toEqual([
      { tag: 'purchasing', count: 2 },
      { tag: 'sap', count: 2 },
      { tag: 'risk/analysis', count: 1 },
      { tag: 'sales', count: 1 },
    ]);
  });
});

describe('describeFilter', () => {
  it('says what filters the tree and how many skills remain, in words', () => {
    expect(describeFilter({}, 5)).toBeUndefined();
    expect(describeFilter({ tag: 'sales' }, 1)).toBe('Filtered by tag: sales — 1 skill');
    expect(describeFilter({ text: 'risk' }, 3)).toBe('Filtered by text: “risk” — 3 skills');
    expect(describeFilter({ tag: 'sap', text: 'order' }, 0)).toBe(
      'Filtered by tag: sap and text: “order” — no skills match',
    );
  });
});
