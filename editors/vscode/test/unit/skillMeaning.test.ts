import { describe, expect, it } from 'vitest';
import {
  autonomyMeaning,
  backendIcon,
  backendMeaning,
  countSkills,
} from '../../src/views/skillMeaning';
import { buildSkillTree } from '../../src/views/skillTree';
import type { Skill } from '../../src/client';

const skill = (over: Partial<Skill>): Skill =>
  ({ id: 's', backend: { kind: 'markdown' }, ...over }) as unknown as Skill;

// 'review', 'sql_view', 'openapi' sat after every skill name as bare jargon, and an external REST
// skill looked just like a local record. The tree now says what they mean, and where data lives.
describe('autonomyMeaning', () => {
  it('translates the gate into what happens to a change', () => {
    expect(autonomyMeaning('review')).toBe('changes need your approval');
    expect(autonomyMeaning('auto')).toBe('changes are applied without review');
    expect(autonomyMeaning('confirm')).toBe('the agent asks you to confirm before it acts');
    expect(autonomyMeaning(undefined)).toBe('changes need your approval');
  });
});

describe('backendMeaning and backendIcon', () => {
  it('names where the data lives, in plain words', () => {
    expect(backendMeaning('sql_view')).toBe('a SQL view (read-only)');
    expect(backendMeaning('openapi')).toBe('a REST service');
    expect(backendMeaning('mcp')).toBe('an MCP server');
    expect(backendMeaning('document')).toBe('uploaded documents');
    expect(backendMeaning('markdown')).toBe('markdown pages in this knowledge base');
    expect(backendMeaning('novel')).toBe('a novel source');
  });

  it('external sources get an icon of their own (cloud, plug, table), local ones keep the role icon', () => {
    expect(backendIcon('openapi')).toBe('cloud');
    expect(backendIcon('mcp')).toBe('plug');
    expect(backendIcon('sql_view')).toBe('table');
    expect(backendIcon('document')).toBe('file-text');
    expect(backendIcon('markdown')).toBeUndefined();
  });
});

describe('countSkills', () => {
  it('counts every skill below a folder, however deep', () => {
    const tree = buildSkillTree([
      skill({ id: 'a', folder: 'sales/orders' }),
      skill({ id: 'b', folder: 'sales/orders' }),
      skill({ id: 'c', folder: 'sales' }),
      skill({ id: 'd', folder: 'purchasing' }),
    ]);
    const sales = tree.find((n) => n.kind === 'folder' && n.label === 'sales')!;
    expect(countSkills(sales)).toBe(3);
    const purchasing = tree.find((n) => n.kind === 'folder' && n.label === 'purchasing')!;
    expect(countSkills(purchasing)).toBe(1);
  });
});
