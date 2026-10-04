import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import { activate, markProcessed, until, wait } from './support';

async function adminCapture(args: Record<string, unknown>): Promise<void> {
  const endpoint = process.env.ESCUREL_TEST_GATEWAY;
  const bearer = process.env.ESCUREL_TEST_ADMIN_BEARER;
  assert.ok(endpoint && bearer);
  const response = await fetch(`${endpoint.replace(/\/+$/, '')}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${bearer}` },
    body: JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'tools/call',
      params: { name: 'capture_event', arguments: args },
    }),
  });
  assert.equal(response.ok, true, `capture_event HTTP ${response.status}`);
  const result = await response.json() as { error?: unknown; result?: { isError?: boolean; content?: unknown } };
  assert.equal(result.error, undefined, JSON.stringify(result.error));
  assert.notEqual(result.result?.isError, true, JSON.stringify(result.result?.content));
}

suite('Evolve problem plan review from the extension host', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    api = await activate();
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
  });

  test('waits for a private exact-revision preflight before filing a plan', async function () {
    this.timeout(120_000);
    const id = `v2-vsix-${Date.now()}`;
    const pageId = `markdown/instances/evolve_problem/${id}.md`;
    const written = await api.services.client.updatePage({
      page_id: pageId,
      content: `---\ntype: instance\nskill: evolve_problem\nid: ${id}\nowner_subject: alice\npilot: p1_decision\nsearch_request: {pilot: p1_decision}\n---\n# Integration review\n`,
      base_sha256: '',
    });
    assert.equal(written.ok, true, JSON.stringify(written.issues));
    const displayed = await api.services.client.expand({ page_id: pageId, raw: true });
    const revision = displayed.content_sha256;
    assert.ok(revision);

    const plan = vscode.commands.executeCommand('escurel.startSkill', {
      skill: 'evolve_run', pageId, mode: 'plan', expectedPageSha256: revision,
    });
    const preflight = await until(async () => {
      const events = await api.services.client.listEvents({ label_skill: 'evolve_preflight', limit: 100 });
      return events.events.find((event) => event.instance_page_id === pageId);
    }, 30_000, 'the extension to capture a private preflight');
    const preflightManual = preflight.provenance?.manual as Record<string, unknown> | undefined;
    assert.equal(preflightManual?.expected_page_sha256, revision);
    const before = await api.services.client.listEvents({ label_skill: 'evolve_run', limit: 100 });
    assert.equal(before.events.some((event) => event.instance_page_id === pageId), false,
      'no plan may be filed before the checked receipt');

    const body = JSON.stringify({
      problem_sha256: revision, structural_ready_for_start: true,
      evidence_scope: 'structure_static_sql_and_holdout_binding_only',
    });
    await adminCapture({
      event_id: `evolve-vsix-preflight-final-${id}`,
      label_skill: 'evolve:preflight', source: 'anofox-evolve', mime: 'application/json',
      kind: 'system', instance_page_id: '', title: 'problem-structure-checked', body,
      provenance: { runner: { root_event_id: preflight.event_id },
        evolve: { phase: 'final', problem_sha256: revision, ready: true } },
    });
    await plan;
    const planned = await until(async () => {
      const events = await api.services.client.listEvents({ label_skill: 'evolve_run', limit: 100 });
      return events.events.find((event) => event.instance_page_id === pageId);
    }, 30_000, 'the extension to file the reviewed plan');
    const plannedManual = planned.provenance?.manual as Record<string, unknown> | undefined;
    assert.equal(plannedManual?.mode, 'plan');
    assert.equal(plannedManual?.target_page_sha256, revision);
    await wait(300);
    await markProcessed(planned.event_id, pageId).catch(() => undefined);
  });
});
