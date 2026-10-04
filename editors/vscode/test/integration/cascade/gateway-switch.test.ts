import * as assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { join } from 'node:path';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, until, wait } from './support';
import { requireEnv } from '../requireEnv';

interface Gateway {
  proc: ChildProcess;
  url: string;
  bearer: string;
}

/** A second REAL gateway process (another tenant), as an operator switching `escurel.gatewayUrl` would meet. */
async function startOtherGateway(): Promise<Gateway> {
  const bin = process.env.ESCUREL_TEST_GATEWAY_BIN;
  assert.ok(bin, 'the harness must provide ESCUREL_TEST_GATEWAY_BIN');
  // The extension's own seed (an empty-inbox tenant): any seed does, the second gateway only has to be a
  // real, different one.
  const ext = vscode.extensions.all.find((e) => e.id.toLowerCase().endsWith('.escurel'));
  assert.ok(ext, 'the extension under test must be installed');
  const seed = join(ext.extensionPath, 'test', 'integration', 'seed');
  const proc = spawn(bin, ['--tenant', 'other', '--seed', seed, '--subject', 'alice'], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const line = await new Promise<string>((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(
      () => reject(new Error('the second gateway did not print its line')),
      60_000,
    );
    proc.stdout!.on('data', (d: Buffer) => {
      buf += d.toString();
      const nl = buf.indexOf('\n');
      if (nl >= 0) {
        clearTimeout(timer);
        resolve(buf.slice(0, nl));
      }
    });
    proc.on('exit', (code) => reject(new Error(`the second gateway exited (${code})`)));
  });
  const info = JSON.parse(line) as { gateway_url: string; bearer: string };
  return { proc, url: info.gateway_url, bearer: info.bearer };
}

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

// An open thread, and the details view showing one of its nodes, belong to the gateway they were read
// from. Switching `escurel.gatewayUrl` (or the tenant) used to leave both holding the OLD tenant's nodes,
// and an action from the details view passed the host's checks against that stale state and then ran
// against the NEW client. Real VS Code, two real gateways.
suite('a gateway switch retires the open threads and the details view', () => {
  let api: EscurelApi;
  let other: Gateway | undefined;

  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    other?.proc.kill('SIGTERM');
    if (api) await discardOpenDrafts(api);
  });

  /** An event run to completion, its thread open, and the details view showing the run node. */
  async function openThreadShowingRun(
    title: string,
  ): Promise<{ rootEventId: string; runId: string }> {
    const page = await freeOrder(api);
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title,
      body: `Meier-Guss: ${title}.`,
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
    await loading;
    api.threads.select(rootEventId, runId);
    const shown = await until(
      async () => api.details.current(),
      15_000,
      'the details to show the run',
    );
    assert.equal(shown.nodeId, runId);
    return { rootEventId, runId };
  }

  /** Point the extension at `url` with `bearer`, the way a person does: setting, sign-in, signal. */
  async function switchTo(url: string, bearer: string): Promise<() => Promise<void>> {
    const cfg = vscode.workspace.getConfiguration('escurel');
    const previousUrl = cfg.get<string>('gatewayUrl');
    const human = process.env.ESCUREL_TEST_BEARER;
    const subject = process.env.ESCUREL_TEST_SUBJECT ?? 'alice';
    await cfg.update('gatewayUrl', url, vscode.ConfigurationTarget.Global);
    api.services.auth.refresher.useStaticToken(bearer, subject);
    api.services.onDidChangeEmit();
    return async () => {
      await cfg.update('gatewayUrl', previousUrl, vscode.ConfigurationTarget.Global);
      if (human) api.services.auth.refresher.useStaticToken(human, subject);
      api.services.onDidChangeEmit();
    };
  }

  async function assertOldNodeIsGone(rootEventId: string): Promise<void> {
    await until(
      async () => (api.details.current() === undefined ? true : undefined),
      15_000,
      'the details view to drop the old tenant’s node',
    );
    // The old thread's id is still a thread this host has a panel for, but its state is gone: an action
    // that would have passed the checks against the stale model is refused.
    const stale = await api.details.handleMessage({
      type: 'details-action',
      rootEventId,
      message: { type: 'view-skill', skill: 'supplier-risk' },
    });
    assert.equal(stale, false, 'an action for the old tenant’s node must be refused');
  }

  test('a second real gateway: nothing of the old tenant can be shown or acted on', async function () {
    this.timeout(240_000);
    const { rootEventId } = await openThreadShowingRun('Gateway switch: confirmation moved');
    other = await startOtherGateway();
    const restore = await switchTo(other.url, other.bearer);
    try {
      await assertOldNodeIsGone(rootEventId);
    } finally {
      // Close the thread panels BEFORE switching back: restoring the URL and the token are two steps, and
      // a panel still open reloads in between with the other gateway's (valid, real) token.
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      await restore();
    }
  });

  // A run panel is the same: its Cancel/Retry/Fix-skill were offered by the old tenant's run.
  test('a run panel opened on the old tenant refuses its actions after a switch', async function () {
    this.timeout(240_000);
    const { runId } = await openThreadShowingRun('Gateway switch: run panel');
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('the run panel did not load')), 30_000);
      const sub = api.runs.onDidLoad(({ runId: id }) => {
        if (id !== runId) return;
        clearTimeout(timer);
        sub.dispose();
        resolve();
      });
      void vscode.commands.executeCommand('escurel.openRun', runId);
    });
    other = other ?? (await startOtherGateway());
    const restore = await switchTo(other.url, other.bearer);
    try {
      await until(
        async () =>
          (await api.runs.handleWebviewMessage(runId, {
            type: 'view-skill',
            skill: 'supplier-risk',
          }))
            ? undefined
            : true,
        15_000,
        'the run panel to refuse an action offered by the old tenant’s run',
      );
    } finally {
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      await restore();
    }
  });

  // The hole the first test does not reach: the reload that follows a switch FAILS (the new gateway is
  // down). The failed load used to leave the old thread, the old inspectors and the selected node in
  // place, so the details view kept offering the old tenant's actions, and they passed the host's checks.
  test('an unreachable gateway: the failed reload does not leave the old tenant on screen', async function () {
    this.timeout(240_000);
    const { rootEventId } = await openThreadShowingRun('Gateway switch: unreachable target');
    // A real closed port: the connection is refused by the operating system.
    const restore = await switchTo('http://127.0.0.1:1', 'not-a-token');
    try {
      await assertOldNodeIsGone(rootEventId);
    } finally {
      // Close the thread panels BEFORE switching back: restoring the URL and the token are two steps, and
      // a panel still open reloads in between with the other gateway's (valid, real) token.
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      await restore();
    }
  });
});
