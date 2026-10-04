// M3's done-when, in a real VS Code against a real gateway that VERIFIES tokens and a real
// runner minting per-run tokens: a cascade E1 → run → changeset → promote → E2 is visible as it
// happens, without a reload, and the run opens with its plan and tool calls.
//
// Nothing here is recorded. Until the gateway verified tokens, this could only be proved as far
// as event plus run: a runner's per-run token was ignored, so the agent's draft carried no run,
// the lineage never showed the changeset, and promoting never cascaded.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { RunView } from '../../../src/shared/protocol';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, until } from './support';
import { requireEnv } from '../requireEnv';

suite('a real cascade', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    if (api) await discardOpenDrafts(api);
  });

  test('E1 → run → changeset → promote → E2 appears on an open thread, and the run shows its plan and calls', async function () {
    this.timeout(240_000);
    const page = await freeOrder(api);
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Supplier risk: Meier-Guss',
      body: 'Meier-Guss downgraded; delivery slips 14 days.',
      instance_page_id: page,
    });
    const root = e1.event_id;

    // The thread is open BEFORE anything past E1 exists to be seen, and every state below is
    // what the HOST loaded on a live event — nobody calls refresh.
    const seen: LoadedThread[] = [];
    const sub = api.threads.onDidLoad((e) => {
      if (e.thread && e.rootEventId === root) seen.push(e.thread);
    });
    try {
      await vscode.commands.executeCommand('escurel.openThread', root);
      const latest = () => seen[seen.length - 1];
      const kinds = () => [...(latest()?.nodes.values() ?? [])].map((n) => n.type).sort();

      // The run held its write: a changeset under the run, a draft under the changeset.
      await until(
        () => (kinds().join() === 'changeset,draft,event,run' ? true : undefined),
        90_000,
        `the thread to hold event, run, changeset and draft (last: ${kinds().join(',') || 'nothing'})`,
      );
      const nodes = [...latest()!.nodes.values()];
      const run = nodes.find((n) => n.type === 'run')!;
      const changeset = nodes.find((n) => n.type === 'changeset')!;
      const draft = nodes.find((n) => n.type === 'draft')!;
      assert.equal(run.parent, root, 'a run hangs off the event that triggered it');
      assert.equal(changeset.parent, run.id, 'the changeset hangs off the run that proposed it');
      assert.equal(draft.parent, changeset.id, 'the draft hangs off its changeset');
      assert.equal(changeset.state, 'open', 'the write is held, not landed');

      // A human promotes, from the same command the canvas button reaches.
      const before = seen.length;
      await vscode.commands.executeCommand('escurel.promote', { changesetId: changeset.id });

      // The cascade hop arrives on the OPEN thread: a second event under the same run.
      await until(
        () => {
          const hop = [...(latest()?.nodes.values() ?? [])].find(
            (n) => n.type === 'event' && n.id !== root,
          );
          return hop ? hop : undefined;
        },
        90_000,
        'the cascade hop to appear on the open thread',
      );
      assert.ok(seen.length > before, 'the thread reloaded by itself');
      const after = [...latest()!.nodes.values()];
      const hop = after.find((n) => n.type === 'event' && n.id !== root)!;
      assert.equal(hop.parent, run.id, 'the hop hangs off the run whose promotion caused it');
      assert.equal(
        hop.label_skill,
        'customer-order',
        "labelled with the produced skill's own name",
      );
      assert.equal(after.find((n) => n.id === changeset.id)?.state, 'promoted');
      assert.equal(after.find((n) => n.id === draft.id)?.state, 'promoted');

      // Run detail: the plan the run reported and the tool calls it made, per call.
      const loaded = new Promise<RunView>((resolve) => {
        const s = api.runs.onDidLoad((e) => {
          if (e.runId === run.id) {
            s.dispose();
            resolve(e.view);
          }
        });
      });
      await vscode.commands.executeCommand('escurel.openRun', run.id);
      const view = await loaded;
      assert.equal(view.status, 'processed');
      assert.ok(view.plan.length > 0, 'the run reported a plan');
      assert.ok(
        view.plan.every((s) =>
          ['pending', 'in_progress', 'completed', 'blocked'].includes(s.status),
        ),
        `plan steps carry a status: ${JSON.stringify(view.plan)}`,
      );
      assert.ok(view.calls.length > 0, 'per-call rows exist now that the token is run-bound');
      assert.equal(
        view.calls.length,
        view.toolCallCount,
        'the rows add up to the count the run reported',
      );
      assert.ok(view.calls.every((c) => c.tool.length > 0 && c.at.endsWith('Z')));
      assert.ok(
        view.calls.some((c) => c.tool === 'create_draft'),
        'it drafted through a tool call',
      );
    } finally {
      sub.dispose();
    }
  });
});
