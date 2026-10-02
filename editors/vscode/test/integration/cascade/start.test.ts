import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, markProcessed, until } from './support';

suite('start a skill: background and plan, approve a plan', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    api = await activate();
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    if (api) await discardOpenDrafts(api);
  });

  test('background start on a free order opens a thread, runs, and produces a changeset', async function () {
    this.timeout(240_000);
    const page = await freeOrder(api);

    const seen: LoadedThread[] = [];
    const sub = api.threads.onDidLoad((e) => {
      if (e.thread) seen.push(e.thread);
    });

    try {
      await vscode.commands.executeCommand('escurel.startSkill', {
        skill: 'customer-order',
        pageId: page,
        mode: 'background',
      });

      const latest = () => seen[seen.length - 1];
      const kinds = () => [...(latest()?.nodes.values() ?? [])].map((n) => n.type).sort();

      // The background run executes: thread opens, run appears, and a changeset appears
      await until(
        () => (kinds().includes('run') && kinds().includes('changeset') ? true : undefined),
        90_000,
        `the thread to hold a run and changeset (last: ${kinds().join(',') || 'nothing'})`,
      );

      // The changeset appears while the run is still going (the draft is written before the run
      // finishes), so wait for the run to settle rather than read its state at that moment.
      await until(
        () => {
          const r = [...(latest()?.nodes.values() ?? [])].find((n) => n.type === 'run');
          return r?.state === 'processed' ? r : undefined;
        },
        30_000,
        'the background run to finish as processed',
      );
      const nodes = [...latest()!.nodes.values()];

      const cs = nodes.find((n) => n.type === 'changeset');
      assert.ok(cs, 'a changeset node must appear under the run');
    } finally {
      sub.dispose();
    }
  });

  test('plan start reaches planned, and approving it sends the approval naming that plan', async function () {
    this.timeout(240_000);
    const page = await freeOrder(api);

    const loads: { rootEventId: string; thread: LoadedThread }[] = [];
    const sub = api.threads.onDidLoad((e) => {
      if (e.thread) loads.push({ rootEventId: e.rootEventId, thread: e.thread });
    });
    const cleanup: string[] = [];

    try {
      await vscode.commands.executeCommand('escurel.startSkill', {
        skill: 'customer-order',
        pageId: page,
        mode: 'plan',
      });

      const planRoot = await until(() => loads[0]?.rootEventId, 30_000, 'the plan thread to open');
      cleanup.push(planRoot);
      const planRun = () =>
        [
          ...(loads
            .filter((l) => l.rootEventId === planRoot)
            .at(-1)
            ?.thread.nodes.values() ?? []),
        ].find((n) => n.type === 'run');

      // The plan run stops in state 'planned' and wrote nothing.
      const planned = await until(
        () => (planRun()?.state === 'planned' ? planRun() : undefined),
        90_000,
        `the plan run to reach 'planned'`,
      );
      const nodes = loads.filter((l) => l.rootEventId === planRoot).at(-1)!.thread.nodes;
      assert.equal(
        [...nodes.values()].some((n) => n.type === 'changeset' || n.type === 'draft'),
        false,
        'a plan writes nothing',
      );

      // Approve it. What is the EXTENSION's to get right is the approval it sends: a user event for
      // the same skill on the same page, naming the plan run. (The echo harness then folds the
      // oldest inbox event with a target page, which is the plan's own, so a changeset under the
      // approval's thread is not something this harness can show; a real harness folds the event
      // it is given.)
      const before = loads.length;
      await vscode.commands.executeCommand('escurel.approvePlan', {
        runId: planned.id,
        skill: 'customer-order',
        pageId: page,
      });
      const approvalRoot = await until(
        () => loads.slice(before).find((l) => l.rootEventId !== planRoot)?.rootEventId,
        30_000,
        'the approval thread to open',
      );
      cleanup.push(approvalRoot);

      const events = await api.services.client.listEvents({ event_id: approvalRoot });
      const approval = events.events.find((e) => e.event_id === approvalRoot);
      assert.ok(approval, 'the approval is a real event');
      assert.equal(approval.label_skill, 'customer-order');
      assert.equal(approval.instance_page_id, page);
      const manual = (approval.provenance as { manual?: Record<string, unknown> } | undefined)
        ?.manual;
      assert.equal(manual?.approved_plan_run_id, planned.id, 'it names the plan run it approves');
      assert.equal(manual?.mode, 'run', 'approving runs the skill for real');
      assert.ok(manual?.requested_by, 'the gateway stamped who asked');
    } finally {
      sub.dispose();
      for (const id of cleanup) await markProcessed(id, page).catch(() => undefined);
    }
  });
});
