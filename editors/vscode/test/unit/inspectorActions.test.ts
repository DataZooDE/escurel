import { describe, expect, it } from 'vitest';
import type { ListLineageResponse, Skill } from '../../src/client/types';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import type { ThreadNode } from '../../src/shared/protocol';
import { buildInspectors } from '../../src/thread/inspector';
import { buildNodeActions, resolveThreadAction } from '../../src/thread/inspectorActions';
import fixtureEventRunChangesetDraft from './fixtures/lineage/lineage-event-run-changeset-draft.json';

const orderSkill: Skill = {
  id: 'order',
  description: 'Manage customer orders',
  actions: [
    { name: 'reassess', kind: 'event', label: 'Reassess risk', event: 'reassess-risk' },
    { name: 'cancel', kind: 'event', label: 'Cancel order', event: 'cancel-order' },
  ],
} as unknown as Skill;

const noteSkill: Skill = {
  id: 'note',
  description: 'Manage notes',
  actions: [{ name: 'summarize', kind: 'event', label: 'Summarize', event: 'summarize-note' }],
} as unknown as Skill;

describe('inspectorActions', () => {
  describe('buildNodeActions', () => {
    it('instance nodes get the skill actions with the skill author’s labels', () => {
      const instanceNode: ThreadNode = {
        id: 'inst-1',
        kind: 'draft',
        parent: null,
        children: [],
        state: null,
        tone: 'neutral',
        title: 'PO-1001',
        meta: [],
        chips: [],
        target: { open: 'page', pageId: 'markdown/instances/order__PO-1001.md' },
        collapsible: false,
      };

      const actions = buildNodeActions(instanceNode, undefined, {
        skills: [orderSkill],
      });

      expect(actions).toBeDefined();
      expect(actions?.skills).toBeDefined();
      expect(actions?.skills?.pageId).toBe('markdown/instances/order__PO-1001.md');
      expect(actions?.skills?.actions).toEqual([
        {
          skill: 'reassess-risk',
          label: 'Reassess risk',
        },
        {
          skill: 'cancel-order',
          label: 'Cancel order',
        },
      ]);
    });

    it('no skills loaded => none (never guess a skill)', () => {
      const instanceNode: ThreadNode = {
        id: 'inst-1',
        kind: 'draft',
        parent: null,
        children: [],
        state: null,
        tone: 'neutral',
        title: 'PO-1001',
        meta: [],
        chips: [],
        target: { open: 'page', pageId: 'markdown/instances/order__PO-1001.md' },
        collapsible: false,
      };

      // With undefined skills
      const noSkills = buildNodeActions(instanceNode, undefined, {});
      expect(noSkills?.skills).toBeUndefined();

      // With empty skills list
      const emptySkills = buildNodeActions(instanceNode, undefined, { skills: [] });
      expect(emptySkills?.skills).toBeUndefined();

      // With unmatching skill
      const unmatching = buildNodeActions(instanceNode, undefined, { skills: [noteSkill] });
      expect(unmatching?.skills).toBeUndefined();
    });

    it('a running/planned/failed/dead-lettered/cancelled/processed run gets the right controls', () => {
      const folded = foldLineage([fixtureEventRunChangesetDraft as unknown as ListLineageResponse]);
      const view = toThreadView(folded);
      const runNode = view.nodes.find((n) => n.kind === 'run')!;
      const rawRunNode = folded.nodes.get(runNode.id)!;

      // 1. running
      const runningActions = buildNodeActions(
        { ...runNode, state: 'running' },
        { ...rawRunNode, state: 'running' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(runningActions?.controls).toMatchObject([
        { action: 'cancel', label: 'Cancel run', enabled: true },
      ]);
      expect(runningActions?.skill).toBe('note');

      // 2. planned
      const plannedActions = buildNodeActions(
        { ...runNode, state: 'planned' },
        { ...rawRunNode, state: 'planned' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(plannedActions?.controls).toMatchObject([
        { action: 'approve', label: 'Approve plan', enabled: true },
      ]);
      expect(plannedActions?.skill).toBe('note');

      // 3. failed
      const failedActions = buildNodeActions(
        { ...runNode, state: 'failed' },
        { ...rawRunNode, state: 'failed' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(failedActions?.controls).toMatchObject([
        { action: 'retry', label: 'Retry', enabled: true },
        { action: 'fix-skill', label: 'Fix skill', enabled: true },
      ]);
      expect(failedActions?.skill).toBe('note');

      // 4. dead_letter
      const deadLetterActions = buildNodeActions(
        { ...runNode, state: 'dead_letter' },
        { ...rawRunNode, state: 'dead_letter' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(deadLetterActions?.controls).toMatchObject([
        { action: 'retry', label: 'Retry', enabled: true },
        { action: 'requeue', label: 'Requeue', enabled: true },
        { action: 'fix-skill', label: 'Fix skill', enabled: true },
      ]);

      // 5. cancelled
      const cancelledActions = buildNodeActions(
        { ...runNode, state: 'cancelled' },
        { ...rawRunNode, state: 'cancelled' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(cancelledActions?.controls).toMatchObject([
        { action: 'retry', label: 'Retry', enabled: true },
      ]);

      // 6. processed
      const processedActions = buildNodeActions(
        { ...runNode, state: 'processed' },
        { ...rawRunNode, state: 'processed' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      expect(processedActions?.controls).toMatchObject([]);
    });

    it('Requeue disabled with a reason for not-admin, enabled for admin and unknown', () => {
      const folded = foldLineage([fixtureEventRunChangesetDraft as unknown as ListLineageResponse]);
      const view = toThreadView(folded);
      const runNode = view.nodes.find((n) => n.kind === 'run')!;
      const rawRunNode = folded.nodes.get(runNode.id)!;

      // not-admin: disabled with reason
      const notAdmin = buildNodeActions(
        { ...runNode, state: 'dead_letter' },
        { ...rawRunNode, state: 'dead_letter' },
        { admin: 'not-admin', lineageNodes: folded.nodes },
      );
      const requeueNotAdmin = notAdmin?.controls?.find((c) => c.action === 'requeue');
      expect(requeueNotAdmin).toMatchObject({
        action: 'requeue',
        label: 'Requeue',
        enabled: false,
        disabledReason: 'Only an admin can requeue a dead letter.',
      });

      // admin: enabled
      const admin = buildNodeActions(
        { ...runNode, state: 'dead_letter' },
        { ...rawRunNode, state: 'dead_letter' },
        { admin: 'admin', lineageNodes: folded.nodes },
      );
      const requeueAdmin = admin?.controls?.find((c) => c.action === 'requeue');
      expect(requeueAdmin).toMatchObject({
        action: 'requeue',
        label: 'Requeue',
        enabled: true,
      });

      // unknown: enabled
      const unknownAdmin = buildNodeActions(
        { ...runNode, state: 'dead_letter' },
        { ...rawRunNode, state: 'dead_letter' },
        { admin: 'unknown', lineageNodes: folded.nodes },
      );
      const requeueUnknown = unknownAdmin?.controls?.find((c) => c.action === 'requeue');
      expect(requeueUnknown).toMatchObject({
        action: 'requeue',
        label: 'Requeue',
        enabled: true,
      });
    });

    it('buildInspectors populates actions for run and instance nodes', () => {
      const folded = foldLineage([fixtureEventRunChangesetDraft as unknown as ListLineageResponse]);
      const view = toThreadView(folded);
      // Add instance node
      view.nodes.push({
        id: 'inst-node-1',
        kind: 'draft',
        parent: null,
        children: [],
        state: null,
        tone: 'neutral',
        title: 'PO-1001',
        meta: [],
        chips: [],
        target: { open: 'page', pageId: 'markdown/instances/order__PO-1001.md' },
        collapsible: false,
      });

      const details = buildInspectors(view, [...folded.nodes.values()], {
        admin: 'not-admin',
        skills: [orderSkill],
      });

      // Instance node got skill actions
      expect(details['inst-node-1']?.actions?.skills).toEqual({
        pageId: 'markdown/instances/order__PO-1001.md',
        actions: [
          { skill: 'reassess-risk', label: 'Reassess risk' },
          { skill: 'cancel-order', label: 'Cancel order' },
        ],
      });

      // Run node got controls
      const runNode = view.nodes.find((n) => n.kind === 'run')!;
      expect(details[runNode.id]?.actions?.controls).toMatchObject([]); // processed run
      expect(details[runNode.id]?.actions?.skill).toBe('note');
    });
  });

  describe('resolveThreadAction', () => {
    function setupThread() {
      const folded = foldLineage([fixtureEventRunChangesetDraft as unknown as ListLineageResponse]);
      const view = toThreadView(folded);
      // Add an instance node to view
      const instanceNode: ThreadNode = {
        id: 'inst-1',
        kind: 'draft',
        parent: null,
        children: [],
        state: null,
        tone: 'neutral',
        title: 'PO-1001',
        meta: [],
        chips: [],
        target: { open: 'page', pageId: 'markdown/instances/order__PO-1001.md' },
        collapsible: false,
      };
      view.nodes.push(instanceNode);
      const runNode = view.nodes.find((n) => n.kind === 'run')!;
      return { folded, view, instanceNode, runNode };
    }

    it('a forged pageId yields undefined', () => {
      const { view, folded } = setupThread();
      const res = resolveThreadAction(
        view,
        {
          type: 'start-skill',
          skill: 'reassess-risk',
          pageId: 'markdown/instances/forged__PO-9999.md',
          mode: 'background',
        },
        { skills: [orderSkill], rawNodes: folded.nodes },
      );
      expect(res).toBeUndefined();
    });

    it('a skill the node does not offer yields undefined', () => {
      const { view, folded } = setupThread();
      const res = resolveThreadAction(
        view,
        {
          type: 'start-skill',
          skill: 'unoffered-skill',
          pageId: 'markdown/instances/order__PO-1001.md',
          mode: 'background',
        },
        { skills: [orderSkill], rawNodes: folded.nodes },
      );
      expect(res).toBeUndefined();
    });

    it('happy path start-skill maps to escurel.startSkill', () => {
      const { view, folded } = setupThread();
      const res = resolveThreadAction(
        view,
        {
          type: 'start-skill',
          skill: 'reassess-risk',
          pageId: 'markdown/instances/order__PO-1001.md',
          mode: 'background',
        },
        { skills: [orderSkill], rawNodes: folded.nodes },
      );
      expect(res).toEqual({
        command: 'escurel.startSkill',
        args: [
          {
            skill: 'reassess-risk',
            pageId: 'markdown/instances/order__PO-1001.md',
            mode: 'background',
          },
        ],
      });
    });

    it('a runId that is not a run in the thread yields undefined', () => {
      const { view, folded } = setupThread();
      const res = resolveThreadAction(
        view,
        {
          type: 'run-control',
          action: 'cancel',
          runId: 'forged-run-id',
        },
        { admin: 'admin', rawNodes: folded.nodes },
      );
      expect(res).toBeUndefined();
    });

    it('an action the run does not offer yields undefined', () => {
      const { view, folded, runNode } = setupThread();
      // run is processed, so it does not offer cancel
      const res = resolveThreadAction(
        view,
        {
          type: 'run-control',
          action: 'cancel',
          runId: runNode.id,
        },
        { admin: 'admin', rawNodes: folded.nodes },
      );
      expect(res).toBeUndefined();
    });

    it('a disabled control yields undefined', () => {
      const { view, folded, runNode } = setupThread();
      runNode.state = 'dead_letter';
      const rawRun = folded.nodes.get(runNode.id)!;
      rawRun.state = 'dead_letter';

      // With not-admin, requeue is disabled
      const res = resolveThreadAction(
        view,
        {
          type: 'run-control',
          action: 'requeue',
          runId: runNode.id,
        },
        { admin: 'not-admin', rawNodes: folded.nodes },
      );
      expect(res).toBeUndefined();
    });

    it('an eventId smuggled in by the webview is ignored, requeue derives eventId from parent', () => {
      const { view, folded, runNode } = setupThread();
      runNode.state = 'dead_letter';
      const rawRun = folded.nodes.get(runNode.id)!;
      rawRun.state = 'dead_letter';

      const parentEventId = runNode.parent!;
      expect(parentEventId).toBeDefined();

      const res = resolveThreadAction(
        view,
        {
          type: 'run-control',
          action: 'requeue',
          runId: runNode.id,
          eventId: 'smuggled-evil-event-id',
        },
        { admin: 'admin', rawNodes: folded.nodes },
      );

      expect(res).toEqual({
        command: 'escurel.requeue',
        args: [{ eventId: parentEventId }],
      });
    });

    it('happy paths for run controls map to the exact commands and args', () => {
      const { view, folded, runNode } = setupThread();
      const rawRun = folded.nodes.get(runNode.id)!;

      // cancel
      runNode.state = 'running';
      rawRun.state = 'running';
      expect(
        resolveThreadAction(
          view,
          { type: 'run-control', action: 'cancel', runId: runNode.id },
          { admin: 'admin', rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.cancelRun',
        args: [{ runId: runNode.id }],
      });

      // retry
      runNode.state = 'failed';
      rawRun.state = 'failed';
      expect(
        resolveThreadAction(
          view,
          { type: 'run-control', action: 'retry', runId: runNode.id },
          { admin: 'admin', rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.retryRun',
        args: [{ runId: runNode.id }],
      });

      // approve (with skill and pageId resolved from thread)
      runNode.state = 'planned';
      rawRun.state = 'planned';
      expect(
        resolveThreadAction(
          view,
          { type: 'run-control', action: 'approve', runId: runNode.id },
          { admin: 'admin', rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.approvePlan',
        args: [
          {
            runId: runNode.id,
            skill: 'note',
            pageId: 'markdown/instances/note/plan.md',
          },
        ],
      });

      // fix-skill
      runNode.state = 'failed';
      rawRun.state = 'failed';
      expect(
        resolveThreadAction(
          view,
          { type: 'run-control', action: 'fix-skill', runId: runNode.id },
          { admin: 'admin', rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.viewSkill',
        args: ['note'],
      });
    });

    it('view-skill: allowed for offered skill, undefined for unoffered', () => {
      const { view, folded } = setupThread();
      // Thread offers 'order' (from instance actions reassess-risk, cancel-order)
      // and 'note' (from run node)

      // Offered run skill
      expect(
        resolveThreadAction(
          view,
          { type: 'view-skill', skill: 'note' },
          { skills: [orderSkill], rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.viewSkill',
        args: ['note'],
      });

      // Offered instance action skill
      expect(
        resolveThreadAction(
          view,
          { type: 'view-skill', skill: 'reassess-risk' },
          { skills: [orderSkill], rawNodes: folded.nodes },
        ),
      ).toEqual({
        command: 'escurel.viewSkill',
        args: ['reassess-risk'],
      });

      // Unoffered skill
      expect(
        resolveThreadAction(
          view,
          { type: 'view-skill', skill: 'alien-skill' },
          { skills: [orderSkill], rawNodes: folded.nodes },
        ),
      ).toBeUndefined();
    });
  });
});
