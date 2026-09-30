// Thread and run detail, end to end in a real VS Code against a real gateway and runner: open
// the thread for a real run and read what the HOST loaded. The webview is out of reach of
// the extension host API by design (postMessage is one way), so the host exposes what it
// loaded and the live-window pass looks at what was drawn.
//
// Static-bearer runner, so the lineage is event + run only (see run.test.ts for why).
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { RunView } from '../../../src/shared/protocol';
import type { LoadedThread } from '../../../src/thread/loadThread';

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

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

suite('thread and run detail', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    for (const d of await api.services.client.listDrafts()) {
      if (d.status === 'open') {
        await api.services.client
          .discardDraft({ draft_id: d.draft_id, reason: 'integration cleanup' })
          .catch(() => undefined);
      }
    }
  });

  test('opening a thread loads its lineage, and its run opens with its facts', async function () {
    this.timeout(180_000);
    const page = 'markdown/instances/supplier-risk__order-4500131.md';
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      body: 'Kessler: delivery risk medium.',
      instance_page_id: page,
    });
    const rootEventId = e1.event_id;

    // Wait for the run to finish so the thread has something to show.
    let runId: string | undefined;
    for (let i = 0; i < 100 && !runId; i += 1) {
      const l = await api.services.client.listLineage({ root_event_id: rootEventId });
      runId = l.nodes.find((n) => n.type === 'run' && n.state !== 'running')?.id;
      if (!runId) await wait(400);
    }
    assert.ok(runId, 'the run must finish');

    // The thread, opened the way the Inbox opens it: by the event row.
    const loading = once<{ rootEventId: string; thread?: LoadedThread }>(api.threads.onDidLoad);
    await vscode.commands.executeCommand('escurel.openThread', rootEventId);
    const { thread } = await loading;
    assert.ok(thread, 'the host must have loaded a thread');
    assert.equal(thread.truncated, false);
    const types = [...thread.nodes.values()].map((n) => n.type).sort();
    assert.deepEqual(types, ['event', 'run']);
    assert.equal(thread.nodes.get(runId)?.parent, rootEventId);

    // A panel exists for it, and a second open reveals it rather than opening another.
    const tabs = () =>
      vscode.window.tabGroups.all
        .flatMap((g) => g.tabs)
        .filter((t) => t.label.startsWith('Thread'));
    assert.equal(tabs().length, 1);
    await vscode.commands.executeCommand('escurel.openThread', rootEventId);
    await wait(500);
    assert.equal(tabs().length, 1, 'the same thread must not open twice');

    // The outline follows the thread that was opened: the root event, with its run beneath.
    // No row says "no thread open", and the empty-state message is gone.
    assert.equal(api.threadsTree.message, undefined);
    const [outlineRoot] = api.threadsTree.getChildren();
    assert.ok(outlineRoot, 'the outline must list the open thread');
    assert.equal(outlineRoot.id, rootEventId);
    assert.equal(outlineRoot.kind, 'event');
    assert.deepEqual(
      outlineRoot.children.map((c) => [c.kind, c.id]),
      [['run', runId]],
    );
    // A row routes through the same command the canvas uses, with the id the command reads.
    const runItem = api.threadsTree.getTreeItem(outlineRoot.children[0]!);
    assert.equal(runItem.command?.command, 'escurel.openRun');
    assert.deepEqual(runItem.command?.arguments, [runId]);

    // Run detail, as the canvas opens it.
    const loadedRun = once<{ runId: string; view: RunView }>(api.runs.onDidLoad);
    await vscode.commands.executeCommand('escurel.openRun', runId);
    const { view } = await loadedRun;
    assert.equal(view.runId, runId);
    assert.equal(view.status, 'processed');
    assert.equal(view.harness, 'echo');
    assert.equal(view.autonomy, 'review');
    assert.ok(view.traceId, 'a copyable trace id');
    // The run REPORTS calls; this harness cannot expose per-call rows (no run-bound token).
    assert.ok((view.toolCallCount ?? 0) > 0, 'the run reports its tool calls');
    assert.equal(view.calls.length, 0, 'per-call detail is not available without run claims');
  });

  test('closing the thread empties the outline again', async function () {
    this.timeout(60_000);
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    const deadline = Date.now() + 10_000;
    while (api.threadsTree.message === undefined && Date.now() < deadline) await wait(200);
    assert.ok(api.threadsTree.message, 'the outline must fall back to its message');
    assert.deepEqual(api.threadsTree.getChildren(), []);
  });

  test('a thread the caller cannot read is an empty thread, not a crash', async function () {
    this.timeout(60_000);
    const loading = once<{ rootEventId: string; thread?: LoadedThread }>(api.threads.onDidLoad);
    await vscode.commands.executeCommand('escurel.openThread', 'no-such-root-event');
    const { thread } = await loading;
    assert.equal(thread?.nodes.size, 0);
  });
});
