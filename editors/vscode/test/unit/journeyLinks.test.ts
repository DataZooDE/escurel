import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import { pageSkill } from '../../src/shared/pageId';
import { nodeLinks, resolveNodeLink } from '../../src/thread/nodeLinks';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import cascade from './fixtures/lineage/lineage-cascade.json';

const fold = (nodes: unknown) => foldLineage([nodes as ListLineageResponse]);
const view = toThreadView(fold(cascade));
const root = view.rootEventId;
const kind = (k: string) => view.nodes.find((n) => n.kind === k)!;

// "Where can I find the analysis in the thread? And I don't see the skills used." A node must say
// which skill it belongs to and link to the things a person asks for next.
describe('pageSkill', () => {
  it('reads the skill of a nested page id and of a flat one', () => {
    expect(pageSkill('markdown/instances/customer-order/order-4500123.md')).toBe('customer-order');
    expect(pageSkill('markdown/instances/supplier-risk-analysis__meier-guss-2026-10-04.md')).toBe(
      'supplier-risk-analysis',
    );
  });
  it('says nothing for a page that is not an instance', () => {
    expect(pageSkill('markdown/skills/customer-order.md')).toBeUndefined();
    expect(pageSkill('')).toBeUndefined();
    expect(pageSkill('markdown/instances/plain.md')).toBeUndefined();
  });
});

describe('thread nodes know their skill, page and run', () => {
  it('a page card names the skill of its record, so an order and an analysis differ', () => {
    const draft = kind('draft');
    expect(draft.skill).toBe('order');
    expect(draft.pageId).toBe('markdown/instances/order/o1.md');
  });
  it('a run card names the skill it executed and the page it worked on', () => {
    const run = kind('run');
    expect(run.skill).toBe('signal');
    expect(run.pageId).toBe('markdown/instances/order/o1.md');
    expect(run.runId).toBe(run.id);
  });
  it('an event names its skill and the page it is about', () => {
    const event = view.nodes.find((n) => n.id === root)!;
    expect(event.skill).toBe('signal');
    expect(event.pageId).toBe('markdown/instances/order/o1.md');
  });
});

describe('what double-click opens on a page card', () => {
  it('a PROMOTED page opens the page itself, not a review with nothing left to decide', () => {
    expect(kind('draft').target).toEqual({
      open: 'page',
      pageId: 'markdown/instances/order/o1.md',
    });
  });
  it('a page still waiting for a decision opens its review', () => {
    const open = JSON.parse(JSON.stringify(cascade)) as { nodes: Record<string, unknown>[] };
    for (const n of open.nodes) if (n.type === 'draft') n.state = 'open';
    const draft = toThreadView(fold(open)).nodes.find((n) => n.kind === 'draft')!;
    expect(draft.target.open).toBe('review');
  });
});

describe('nodeLinks', () => {
  it('a run links to its skill, its page and its own detail, and to the thread', () => {
    expect(nodeLinks(kind('run'), root).map((l) => l.id)).toEqual([
      'skill',
      'page',
      'run',
      'thread',
    ]);
  });
  it('a promoted page links to its skill, the page and the run that wrote it', () => {
    expect(nodeLinks(kind('draft'), root).map((l) => l.id)).toEqual(['skill', 'page', 'run']);
  });
  it('an open changeset links to the run that proposed it, and to its review', () => {
    const open = JSON.parse(JSON.stringify(cascade)) as { nodes: Record<string, unknown>[] };
    for (const n of open.nodes) if (n.type === 'changeset') n.state = 'open';
    const v = toThreadView(fold(open));
    expect(
      nodeLinks(
        v.nodes.find((n) => n.kind === 'changeset')!,
        root,
      ).map((l) => l.id),
    ).toEqual(['run', 'review']);
  });
  it('labels say what opens, never an id', () => {
    for (const l of nodeLinks(kind('run'), root)) expect(l.label).not.toMatch(/[0-9A-Z]{20,}/);
    expect(nodeLinks(kind('run'), root)[0]!.label).toBe('View skill: signal');
  });
  it('the root event does not offer to open the thread it is already in', () => {
    const event = view.nodes.find((n) => n.id === root)!;
    expect(nodeLinks(event, root).map((l) => l.id)).not.toContain('thread');
  });
});

// The webview is not trusted: it names a node and a link KIND; the host decides what that opens.
describe('resolveNodeLink', () => {
  it('turns a link kind into the command that owns that surface, from the host’s own view', () => {
    const run = kind('run');
    expect(resolveNodeLink(view, root, run.id, 'skill')).toEqual({
      command: 'escurel.viewSkill',
      args: ['signal'],
    });
    expect(resolveNodeLink(view, root, run.id, 'page')).toEqual({
      command: 'escurel.openInstance',
      args: ['markdown/instances/order/o1.md'],
    });
    expect(resolveNodeLink(view, root, run.id, 'run')).toEqual({
      command: 'escurel.openRun',
      args: [run.id],
    });
    expect(resolveNodeLink(view, root, run.id, 'thread')).toEqual({
      command: 'escurel.openThread',
      args: [root],
    });
  });
  it('refuses a node that is not in the thread, a link the node does not offer, and junk', () => {
    expect(resolveNodeLink(view, root, 'forged', 'skill')).toBeUndefined();
    expect(resolveNodeLink(view, root, kind('draft').id, 'thread')).toBeUndefined();
    expect(resolveNodeLink(view, root, kind('run').id, 'delete' as never)).toBeUndefined();
  });
});
