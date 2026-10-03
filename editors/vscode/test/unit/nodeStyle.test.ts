import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import { describeNodeType } from '../../src/thread/nodeStyle';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';

const fixture = (name: string) =>
  JSON.parse(
    readFileSync(join(__dirname, 'fixtures', 'lineage', name), 'utf8'),
  ) as ListLineageResponse;

const view = toThreadView(foldLineage([fixture('lineage-cascade.json')]));
const byId = (id: string) => view.nodes.find((n) => n.id === id)!;

// Cards used to look alike: a coloured dot of one of four tones, shared by the changeset and its
// pages. Each type now has its own word, icon and accent, so colour is never the only cue.
describe('describeNodeType', () => {
  it('gives every kind its own word, icon and accent', () => {
    const root = byId(view.rootEventId);
    const cascade = view.nodes.find((n) => n.kind === 'event' && n.id !== view.rootEventId)!;
    const run = view.nodes.find((n) => n.kind === 'run')!;
    const changeset = view.nodes.find((n) => n.kind === 'changeset')!;
    const draft = view.nodes.find((n) => n.kind === 'draft')!;
    const described = [root, cascade, run, changeset, draft].map((n) =>
      describeNodeType(n, view.rootEventId),
    );
    expect(described.map((d) => d.label)).toEqual(['event', 'cascade', 'run', 'changeset', 'page']);
    expect(new Set(described.map((d) => d.icon)).size).toBe(5);
    expect(new Set(described.map((d) => d.accent)).size).toBe(5);
  });
});

// A finished node with nothing left to do takes little room; anything that is waiting or live keeps it.
describe('emphasis', () => {
  it('shrinks finished nodes and keeps live and waiting ones full size', () => {
    const emphasis = (kind: string, state: string) =>
      view.nodes.find((n) => n.kind === kind && n.state === state)?.emphasis;
    expect(emphasis('event', 'processed')).toBe('compact'); // the root
    expect(emphasis('run', 'processed')).toBe('compact');
    expect(emphasis('changeset', 'promoted')).toBe('compact');
    expect(emphasis('draft', 'promoted')).toBe('compact');
    expect(emphasis('event', 'inbox')).toBe('normal'); // the cascade event is waiting
  });

  it('keeps a running node at full size, and gives what waits on a person the strongest card', () => {
    const lineage = (type: string, state: string): ListLineageResponse => ({
      root_event_id: 'e',
      nodes: [
        { id: 'e', type: 'event', parent: null, state: 'inbox' },
        { id: 'n', type: type, parent: 'e', state },
      ],
    });
    const of = (type: string, state: string) =>
      toThreadView(foldLineage([lineage(type, state)])).nodes.find((n) => n.id === 'n')!.emphasis;
    expect(of('run', 'running')).toBe('normal');
    expect(of('run', 'failed')).toBe('needs-you');
    expect(of('run', 'dead_letter')).toBe('needs-you');
    expect(of('run', 'planned')).toBe('needs-you');
    expect(of('changeset', 'open')).toBe('needs-you');
    expect(of('draft', 'open')).toBe('needs-you');
    expect(of('changeset', 'discarded')).toBe('compact');
    expect(of('run', 'cancelled')).toBe('compact');
  });
});
