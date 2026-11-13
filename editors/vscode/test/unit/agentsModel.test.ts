import { describe, expect, it } from 'vitest';
import type { Instance, UsageReportResponse } from '../../src/client/types';
import { agentProfile, agentRows } from '../../src/views/agentsModel';

// An agent profile is an instance of the `agent` system skill (references/01, "Agent profiles");
// usage_report {group_by: [agent]} keys a run by `agent:<id>`.
const scribe: Instance = {
  page_id: 'markdown/instances/agent/scribe.md',
  skill: 'agent',
  at: '2026-11-13T05:00:00Z',
  frontmatter: {
    kind: 'instance',
    skill: 'agent',
    id: 'scribe',
    harness: 'claude',
    model: 'claude-sonnet-4',
    skills: ['[[skill::note]]', '[[skill::triage]]'],
    scopes: { tools: ['search', 'expand', 'update_page'], write: ['markdown/instances/note/*'] },
    max_concurrent: 2,
  },
};
const bare: Instance = {
  page_id: 'markdown/instances/agent/reader.md',
  skill: 'agent',
  at: null,
  frontmatter: { id: 'reader', skills: '[[skill::note]]' },
};

const usage: UsageReportResponse = {
  group_by: ['agent'],
  rows: [
    {
      group: { agent: 'agent:scribe' },
      runs: 3,
      by_status: { processed: 2, failed: 1 },
      input_tokens: 10,
      output_tokens: 5,
      cost_usd: 0.01,
    },
    {
      // A run without a profile groups as its skill: not one of the listed agents.
      group: { agent: 'agent:note' },
      runs: 9,
      by_status: { processed: 9 },
      input_tokens: 1,
      output_tokens: 1,
      cost_usd: 0,
    },
  ],
  total: {
    runs: 12,
    by_status: { processed: 11, failed: 1 },
    input_tokens: 11,
    output_tokens: 6,
    cost_usd: 0.01,
  },
};

describe('agents view model', () => {
  it('reads a profile off the agent page frontmatter', () => {
    expect(agentProfile(scribe)).toEqual({
      id: 'scribe',
      pageId: 'markdown/instances/agent/scribe.md',
      harness: 'claude',
      model: 'claude-sonnet-4',
      skills: ['note', 'triage'],
      tools: ['search', 'expand', 'update_page'],
      write: ['markdown/instances/note/*'],
      maxConcurrent: 2,
    });
    // Absent scopes mean "no extra limit", not "nothing allowed".
    expect(agentProfile(bare)).toEqual({
      id: 'reader',
      pageId: 'markdown/instances/agent/reader.md',
      skills: ['note'],
    });
  });

  it('summarises harness, model, scopes and its runs from usage', () => {
    const [row] = agentRows([agentProfile(scribe)], { usage, window: 'Last 30 days' });
    expect(row).toMatchObject({
      label: 'scribe',
      description: 'claude · claude-sonnet-4 · 3 runs · 67% ok',
      pageId: 'markdown/instances/agent/scribe.md',
    });
    expect(row!.tooltip).toContain('Runs: note, triage');
    expect(row!.tooltip).toContain('Tools: search, expand, update_page');
    expect(row!.tooltip).toContain('Writes: markdown/instances/note/*');
    expect(row!.tooltip).toContain('At most 2 at once');
    expect(row!.tooltip).toContain('Last 30 days: 3 runs · 67% ok');
  });

  it('says no runs for an agent with none, and nothing about runs without an admin', () => {
    const [none] = agentRows([agentProfile(bare)], { usage, window: 'Last 30 days' });
    expect(none!.description).toBe('runner default agent · no runs');
    expect(none!.tooltip).toContain('Tools: no limit beyond the skill');
    const [noAdmin] = agentRows([agentProfile(scribe)], { usage: 'admin-required', window: 'x' });
    expect(noAdmin!.description).toBe('claude · claude-sonnet-4');
    expect(noAdmin!.tooltip).toContain('Run counts need an admin sign-in.');
  });

  it('lists agents by id', () => {
    const rows = agentRows([agentProfile(scribe), agentProfile(bare)], { usage, window: 'w' });
    expect(rows.map((r) => r.label)).toEqual(['reader', 'scribe']);
  });
});
