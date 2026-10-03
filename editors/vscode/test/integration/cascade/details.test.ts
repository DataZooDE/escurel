import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, until, wait } from './support';

function once<T>(event: vscode.Event<T>, ms = 30_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error('timed out waiting for the host to load'));
    }, ms);
    const sub = event((v) => {
      clearTimeout(timer);
      sub.dispose();
      resolve(v);
    });
  });
}

// The details of the selected node are a view of their own in the panel area. A real VS Code, a real
// gateway and a real runner: the host decides what the view shows and what it may do, and a message
// from the view is acted on only for the thread it is showing.
suite('the details view follows the selected node and acts only for its thread', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    api = await activate();
  });

  teardown(async () => {
    if (api) await discardOpenDrafts(api);
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    if (api) await discardOpenDrafts(api);
  });

  test('selecting a node shows it, a forged thread id is refused, closing the thread clears it', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      // Distinct from every other test's signal (the runner skips identical content).
      title: 'Details view: confirmation moved',
      body: 'Meier-Guss: PO confirmation moved by 11 days.',
      instance_page_id: page,
    });
    const rootEventId = e1.event_id;

    let runId: string | undefined;
    for (let i = 0; i < 100 && !runId; i += 1) {
      const l = await api.services.client.listLineage({ root_event_id: rootEventId });
      const run = l.nodes.find((n) => n.type === 'run' && n.state !== 'running');
      if (l.nodes.some((n) => n.type === 'changeset') && run) runId = run.id;
      else await wait(400);
    }
    assert.ok(runId, 'the run and changeset must finish');

    const loading = once<{ rootEventId: string; thread?: LoadedThread }>(api.threads.onDidLoad);
    await vscode.commands.executeCommand('escurel.openThread', rootEventId);
    const { thread } = await loading;
    assert.ok(thread, 'the host must have loaded a thread');
    assert.equal(api.details.current(), undefined, 'nothing is shown before a selection');

    // 1. A selection (here from the outline, which uses the same host path as a canvas click)
    //    shows that node, with the thread it belongs to and its inspector.
    api.threads.select(rootEventId, runId);
    const shown = await until(
      async () => api.details.current(),
      15_000,
      'the details view to be given the selected run',
    );
    assert.equal(shown.rootEventId, rootEventId);
    assert.equal(shown.nodeId, runId);
    assert.ok(shown.detail.title.length > 0, 'it carries the node’s inspector');

    // 2. A message for another thread is refused, even for an action the inspector has.
    const forged = await api.details.handleMessage({
      type: 'details-action',
      rootEventId: 'not-a-thread-that-is-shown',
      message: { type: 'view-skill', skill: 'supplier-risk' },
    });
    assert.equal(forged, false, 'a forged thread id must be refused');

    // 3. Something that is not an inspector action is refused for the right thread too.
    const notAnAction = await api.details.handleMessage({
      type: 'details-action',
      rootEventId,
      message: { type: 'promote', changesetId: 'whatever' },
    });
    assert.equal(notAnAction, false, 'only the three inspector actions are accepted');

    // 4. A real inspector action for the shown thread goes through the thread's own validation: an
    //    unoffered control on a finished run is still refused there.
    const unoffered = await api.details.handleMessage({
      type: 'details-action',
      rootEventId,
      message: { type: 'run-control', action: 'cancel', runId },
    });
    assert.equal(
      unoffered,
      false,
      'the thread re-validates: cancel is not offered for a finished run',
    );

    // 5. Closing the thread leaves nothing to show.
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    await until(
      async () => (api.details.current() === undefined ? true : undefined),
      15_000,
      'the details view to clear when its thread closes',
    );
  });
});
