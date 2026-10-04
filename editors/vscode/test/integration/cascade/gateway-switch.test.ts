import * as assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { LoadedThread } from '../../../src/thread/loadThread';
import { activate, discardOpenDrafts, freeOrder, until, wait } from './support';

interface Gateway {
  proc: ChildProcess;
  url: string;
  bearer: string;
}

/** A second REAL gateway process (another tenant), as an operator switching `escurel.gatewayUrl` would meet. */
async function startOtherGateway(): Promise<Gateway> {
  const bin = process.env.ESCUREL_TEST_GATEWAY_BIN;
  assert.ok(bin, 'the harness must provide ESCUREL_TEST_GATEWAY_BIN');
  const proc = spawn(bin, ['--tenant', 'other', '--subject', 'alice'], {
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
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    api = await activate();
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    other?.proc.kill('SIGTERM');
    if (api) await discardOpenDrafts(api);
  });

  test('after the switch nothing of the old tenant can be shown or acted on', async function () {
    this.timeout(240_000);
    const page = await freeOrder(api);
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Gateway switch: confirmation moved',
      body: 'Meier-Guss: PO confirmation moved by 13 days.',
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

    // Switch to a second real gateway, the way a person does: the setting, the sign-in, the signal.
    other = await startOtherGateway();
    const cfg = vscode.workspace.getConfiguration('escurel');
    const previousUrl = cfg.get<string>('gatewayUrl');
    const human = process.env.ESCUREL_TEST_BEARER;
    const subject = process.env.ESCUREL_TEST_SUBJECT ?? 'alice';
    try {
      await cfg.update('gatewayUrl', other.url, vscode.ConfigurationTarget.Global);
      api.services.auth.refresher.useStaticToken(other.bearer, subject);
      api.services.onDidChangeEmit();

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
    } finally {
      await cfg.update('gatewayUrl', previousUrl, vscode.ConfigurationTarget.Global);
      if (human) api.services.auth.refresher.useStaticToken(human, subject);
      api.services.onDidChangeEmit();
    }
  });
});
