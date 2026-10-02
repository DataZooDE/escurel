import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, markProcessed, until, wait } from './support';

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

suite('thread inspector actions: start-skill and run controls', () => {
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

  test('host handles inspector actions: rejects forged start-skill and run-control, accepts real start-skill', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      // Distinct from every other test's signal: the runner's gate skips a signal whose content is
      // identical to one already folded into the same instance, and no run is created at all.
      title: 'Thread actions: confirmation moved',
      body: 'Meier-Guss: PO confirmation moved by 9 days.',
      instance_page_id: page,
    });
    const rootEventId = e1.event_id;

    // Wait for the run and changeset to appear so thread has both run and draft
    let runId: string | undefined;
    for (let i = 0; i < 100 && !runId; i += 1) {
      const l = await api.services.client.listLineage({ root_event_id: rootEventId });
      const hasChangeset = l.nodes.some((n) => n.type === 'changeset');
      const run = l.nodes.find((n) => n.type === 'run' && n.state !== 'running');
      if (hasChangeset && run) {
        runId = run.id;
      } else {
        await wait(400);
      }
    }
    assert.ok(runId, 'the run and changeset must finish');

    // Open the thread
    const loading = once<{ rootEventId: string; thread?: LoadedThread }>(api.threads.onDidLoad);
    await vscode.commands.executeCommand('escurel.openThread', rootEventId);
    const { thread } = await loading;
    assert.ok(thread, 'the host must have loaded a thread');

    // 1. Forged start-skill: forged pageId does nothing
    const forgedPageHandled = await api.threads.handleWebviewMessage(rootEventId, {
      type: 'start-skill',
      skill: 'customer-order',
      pageId: 'markdown/instances/customer-order__nonexistent-404.md',
      mode: 'background',
    });
    assert.equal(forgedPageHandled, false, 'forged pageId must be rejected');

    // 2. Forged start-skill: skill not offered by the node does nothing
    const forgedSkillHandled = await api.threads.handleWebviewMessage(rootEventId, {
      type: 'start-skill',
      skill: 'nonexistent-skill-xyz',
      pageId: page,
      mode: 'background',
    });
    assert.equal(forgedSkillHandled, false, 'unoffered skill must be rejected');

    // 3. Forged run-control: non-existent runId does nothing
    const forgedRunHandled = await api.threads.handleWebviewMessage(rootEventId, {
      type: 'run-control',
      action: 'cancel',
      runId: 'fake-run-id-999',
    });
    assert.equal(forgedRunHandled, false, 'forged runId must be rejected');

    // 4. Forged run-control: control not offered for a processed run (e.g. cancel on completed run) does nothing
    const forgedActionHandled = await api.threads.handleWebviewMessage(rootEventId, {
      type: 'run-control',
      action: 'cancel',
      runId,
    });
    assert.equal(forgedActionHandled, false, 'unoffered run-control action must be rejected');

    // 5. Real start-skill captures an event (assert it appears via the client)
    // By skill ALONE, then filtered by page here. Filtering by page asks for the page's HISTORY
    // (processed events), which narrows to nothing for a start that is still in the inbox; the
    // gateway answers `[]` for skill + page and for page alone (checked against a real gateway).
    const startedOn = async () => {
      const all = await api.services.client.listEvents({
        label_skill: 'supplier-risk',
        newest_first: true,
        limit: 20,
      });
      return { events: all.events.filter((e) => e.instance_page_id === page) };
    };
    // The page holds the first run's open draft, and a page takes one draft at a time: a second run
    // for it would be refused and dead-lettered, and its event would sit in the inbox as the OLDEST
    // with a target, swallowing later tests' runs. Free the page first.
    await discardOpenDrafts(api);
    // And the first run's own event is still in the inbox (a review run leaves it there until its
    // draft is promoted); the echo harness folds the OLDEST such event, so it would be folded again
    // instead of the one about to be started. It is done with: mark it processed.
    await markProcessed(rootEventId, page);
    const beforeEvents = await startedOn();

    const realHandled = await api.threads.handleWebviewMessage(rootEventId, {
      type: 'start-skill',
      skill: 'supplier-risk',
      pageId: page,
      mode: 'background',
    });
    assert.equal(realHandled, true, 'real start-skill must be handled');

    // Assert the event appears via the client
    const afterEvent = await until(
      async () => {
        const events = await startedOn();
        return events.events.find(
          (e) =>
            e.label_skill === 'supplier-risk' &&
            e.instance_page_id === page &&
            !beforeEvents.events.some((be) => be.event_id === e.event_id),
        );
      },
      30_000,
      'the newly captured start-skill event to appear via client',
    );
    assert.ok(afterEvent, 'captured event must be found via the client');
    assert.equal(afterEvent.label_skill, 'supplier-risk');
    assert.equal(afterEvent.instance_page_id, page);
    // Let that run finish (it drafts a change for the page), then leave nothing behind: discard the
    // draft and mark both events processed, so no event is left in the inbox for the next test.
    await until(
      async () => {
        const drafts = await api.services.client.listDrafts();
        return drafts.find((d) => d.status === 'open' && d.event_id === afterEvent.event_id);
      },
      60_000,
      'the started run to draft its change',
    );
    await discardOpenDrafts(api);
    await markProcessed(afterEvent.event_id, page);
  });
});
