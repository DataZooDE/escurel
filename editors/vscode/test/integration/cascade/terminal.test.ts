import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { RunView } from '../../../src/shared/protocol';
import { activate, freeOrder, until } from './support';

suite('start in terminal', () => {
  let api: EscurelApi;
  const setting = vscode.workspace.getConfiguration('escurel');
  let previous: string | undefined;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    api = await activate();
  });

  teardown(async () => {
    await setting.update('shellHarness', previous, vscode.ConfigurationTarget.Global);
    for (const terminal of vscode.window.terminals.filter((t) => t.name.startsWith('escurel: '))) {
      terminal.dispose();
    }
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });

  test('a minted terminal run appears in lineage after the harness calls the gateway', async function () {
    this.timeout(120_000);
    previous = setting.inspect<string>('shellHarness')?.globalValue;

    const code =
      "const url=new URL('/mcp',process.env.ESCUREL_URL);" +
      "const body={jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'report_progress',arguments:{plan:[{step:'terminal start',status:'in_progress'}]}}};" +
      "fetch(url,{method:'POST',headers:{'content-type':'application/json',authorization:'Bearer '+process.env.ESCUREL_TOKEN,accept:'application/json, text/event-stream'},body:JSON.stringify(body)})" +
      // No `!` in the command: an interactive shell expands `!r` as history ('event not found')
      // even inside double quotes, and the whole line is silently abandoned.
      '.then(r=>{r.ok||(process.exitCode=1)})' +
      '.catch(()=>{process.exitCode=1})';
    await setting.update(
      'shellHarness',
      `node -e ${JSON.stringify(code)}`,
      vscode.ConfigurationTarget.Global,
    );

    const pageId = await freeOrder(api);
    const minted = await vscode.commands.executeCommand<{ runId: string; rootEventId: string }>(
      'escurel.startInTerminal',
      { skill: 'supplier-risk', pageId },
    );
    assert.ok(minted?.runId && minted.rootEventId, 'the command must mint a governed run');

    await until(
      async () => {
        const events = await api.services.client.listEvents({
          run_id: minted.runId,
          include_system: true,
        });
        return events.events.some((event) => event.title === 'run-progress') ? true : undefined;
      },
      45_000,
      'the terminal harness to report progress',
    );

    // Run detail is where a run with no root event shows: the host loads it from `list_events{run_id}`.
    // Its thread would be EMPTY by design (a synthetic root reads as absent), so the command opens
    // the run, and the plan the terminal's harness reported is on it.
    const view = await until(
      async () => {
        const loaded = await new Promise<RunView | undefined>((resolve) => {
          const sub = api.runs.onDidLoad((e) => {
            if (e.runId === minted.runId) {
              sub.dispose();
              resolve(e.view);
            }
          });
          void vscode.commands.executeCommand('escurel.openRun', minted.runId);
          setTimeout(() => {
            sub.dispose();
            resolve(undefined);
          }, 8_000);
        });
        return loaded?.plan.some((s) => s.step === 'terminal start') ? loaded : undefined;
      },
      45_000,
      'run detail to show the plan the terminal reported',
    );
    assert.equal(view.runId, minted.runId);
    assert.equal(view.status, 'running', 'the token has not lapsed and nothing closed the run');
  });
});
