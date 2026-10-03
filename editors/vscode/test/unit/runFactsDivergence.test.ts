import { describe, expect, it } from 'vitest';
import type { LineageNode, ListLineageResponse } from '../../src/client/types';
import { buildNodeActions, resolveThreadAction } from '../../src/thread/inspectorActions';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import { resolveRunAction } from '../../src/runs/runActions';
import type { ActionRunView } from '../../src/runs/runActions';
import { triggerSkill } from '../../src/runs/runFacts';
import fixture from './fixtures/lineage/lineage-event-run-changeset-draft.json';

// The thread inspector and the run page offer the same controls for the same run and resolve them
// into the same commands. They had DIVERGED: the thread path guessed ids the run page refuses to.
function thread(mutate?: (nodes: Map<string, LineageNode>) => void, state = 'dead_letter') {
  const folded = foldLineage([fixture as unknown as ListLineageResponse]);
  const view = toThreadView(folded);
  const runNode = view.nodes.find((n) => n.kind === 'run')!;
  const raw = folded.nodes.get(runNode.id)!;
  runNode.state = state;
  raw.state = state;
  mutate?.(folded.nodes);
  return { folded, view, runNode, raw };
}
const controlsOf = (t: ReturnType<typeof thread>, admin: 'admin' | 'not-admin' = 'admin') =>
  buildNodeActions(t.runNode, t.raw, { admin, rawById: t.folded.nodes })?.controls?.map(
    (c) => c.action,
  );
const resolve = (t: ReturnType<typeof thread>, action: string) =>
  resolveThreadAction(t.view, { type: 'run-control', action, runId: t.runNode.id } as never, {
    admin: 'admin',
    rawNodes: t.folded.nodes,
  });

describe('requeue names the TRIGGER event, which must be a real event of the thread', () => {
  it('is offered and resolved when the run’s lineage parent is an event', () => {
    const t = thread();
    expect(controlsOf(t)).toContain('requeue');
    expect(resolve(t, 'requeue')).toEqual({
      command: 'escurel.requeue',
      args: [{ eventId: t.raw.parent }],
    });
  });

  it('is neither offered nor resolved when that event is not in the lineage (pruned), instead of naming the thread view’s guessed parent', () => {
    const t = thread((nodes) => nodes.delete(t0Parent(nodes)));
    expect(controlsOf(t)).not.toContain('requeue');
    expect(resolve(t, 'requeue')).toBeUndefined();
  });
});
function t0Parent(nodes: Map<string, LineageNode>): string {
  return [...nodes.values()].find((n) => n.type === 'run')!.parent!;
}

describe('approve needs the page the run targeted, not a guess', () => {
  it('is not offered when the run names no target page (a produced instance or a parent’s page is not that)', () => {
    const t = thread((nodes) => {
      const run = [...nodes.values()].find((n) => n.type === 'run')!;
      delete run.target_page_id; // produced_instance and the parent's instance_page_id stay
    }, 'planned');
    expect(controlsOf(t)).not.toContain('approve');
    expect(resolve(t, 'approve')).toBeUndefined();
  });

  it('is offered and resolved with the run’s own target page', () => {
    const t = thread(undefined, 'planned');
    expect(controlsOf(t)).toContain('approve');
    expect(resolve(t, 'approve')).toEqual({
      command: 'escurel.approvePlan',
      args: [{ runId: t.runNode.id, skill: 'note', pageId: 'markdown/instances/note/plan.md' }],
    });
  });
});

describe('the run’s skill is the skill of the event that triggered it, never another root', () => {
  it('does not borrow an unrelated parentless event’s skill when the trigger is pruned', () => {
    const t = thread((nodes) => {
      nodes.delete(t0Parent(nodes));
      nodes.set('stray', {
        id: 'stray',
        type: 'event',
        parent: null,
        state: 'x',
        label_skill: 'other-skill',
      });
    }, 'failed');
    expect(controlsOf(t)).not.toContain('fix-skill');
    expect(resolve(t, 'fix-skill')).toBeUndefined();
  });

  it('triggerSkill reads the run’s parent event by id', () => {
    const nodes: LineageNode[] = [
      { id: 'root', type: 'event', parent: null, state: 'processed', label_skill: 'supplier-risk' },
      { id: 'run1', type: 'run', parent: 'root', state: 'processed' },
      {
        id: 'cascade',
        type: 'event',
        parent: 'run1',
        state: 'processed',
        label_skill: 'customer-order',
      },
      { id: 'run2', type: 'run', parent: 'cascade', state: 'failed' },
    ];
    // The follow-on run belongs to customer-order, not to the thread's root skill.
    expect(triggerSkill(nodes, 'run2')).toBe('customer-order');
    expect(triggerSkill(nodes, 'run1')).toBe('supplier-risk');
    expect(triggerSkill(nodes, 'missing')).toBeUndefined();
    expect(triggerSkill([nodes[1]!], 'run1')).toBeUndefined();
  });
});

describe('both paths resolve the same facts to the same command', () => {
  it('cancel / retry / requeue / approve / fix-skill agree', () => {
    for (const [state, action] of [
      ['running', 'cancel'],
      ['failed', 'retry'],
      ['dead_letter', 'requeue'],
      ['planned', 'approve'],
      ['failed', 'fix-skill'],
    ] as const) {
      const t = thread(undefined, state);
      const viaThread = resolve(t, action);
      const view = {
        runId: t.runNode.id,
        skill: 'note',
        targetPageId: 'markdown/instances/note/plan.md',
        triggerEventId: t.raw.parent!,
        controls: buildNodeActions(t.runNode, t.raw, { admin: 'admin', rawById: t.folded.nodes })
          ?.controls,
      } as unknown as ActionRunView;
      const viaRun = resolveRunAction(view, { type: 'run-control', action, runId: view.runId });
      expect(viaThread, action).toBeDefined();
      expect(viaRun, action).toBeDefined();
      expect(viaThread!.args, action).toEqual([viaRun!.args]);
      expect(viaThread!.command, action).toBe(viaRun!.command);
    }
  });
});
