import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { Event } from '../../../src/client/types';
import type { RunView } from '../../../src/shared/protocol';
import type { EscurelApi } from '../../../src/extension';
import { activate, discardOpenDrafts, freeOrder, signInAsAdmin, until } from '../cascade/support';
import { requireEnv } from '../requireEnv';
import { chipsForSkill } from '../../../src/views/knowledgeModel';

// The real controls runner (runTests.ts: ESCUREL_RUNNER_ID=vsx-controls, labels repo:escurel and
// site:vsx, echo sleeping 4 s per run) registers its `runtime` page on boot, reports status, and
// claims each inbox event it takes (#818). Nothing here is seeded but a skill's `requires:`.
const RUNNER = 'vsx-controls';

async function runStarted(api: EscurelApi, eventId: string): Promise<Event> {
  return until(
    async () =>
      (
        await api.services.client.listEvents({
          label_skill: 'escurel:run',
          include_system: true,
          newest_first: true,
          limit: 100,
        })
      ).events.find(
        (e) =>
          e.title === 'run-started' &&
          (e.provenance?.runner as Record<string, unknown> | undefined)?.event_id === eventId,
      ),
    60_000,
    'run-started',
  );
}

function liveView(
  api: EscurelApi,
  runId: string,
  accept: (v: RunView) => boolean,
): Promise<RunView> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error('run detail never showed who holds the event'));
    }, 60_000);
    const sub = api.runs.onDidLoad(({ runId: id, view }) => {
      if (id !== runId || !accept(view)) return;
      clearTimeout(timer);
      sub.dispose();
      resolve(view);
    });
    void vscode.commands.executeCommand('escurel.openRun', runId);
  });
}

suite('runtimes against the real stack', () => {
  let api: EscurelApi;
  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
    await discardOpenDrafts(api);
  });
  teardown(async function () {
    this.timeout(60_000);
    await discardOpenDrafts(api);
  });

  test('the Runtimes view lists the running runner online, with its harness and labels', async function () {
    this.timeout(90_000);
    // As the member the extension signs in as: the gateway may leave out CLI versions and holders.
    const row = await until(
      async () => {
        await api.runtimes.load();
        return api.runtimes.rows().find((r) => r.label === RUNNER && r.status === 'online');
      },
      60_000,
      'the controls runner online',
    );
    // The echo harness reports a version; a member's read may leave it out (admins only).
    assert.match(row.description, /^online · echo( [^·]+)? · repo:escurel, site:vsx · /);
    assert.match(row.tooltip, /^Online: last seen /m);
    assert.equal(row.pageId, 'markdown/instances/runtime/vsx-controls.md');
    assert.match(api.runtimes.message ?? '', /^\d+ of \d+ online$/);
  });

  test('run detail names the runtime that holds a live run’s event', async function () {
    this.timeout(180_000);
    const back = signInAsAdmin(api);
    try {
      const trigger = await api.services.client.captureEvent({
        label_skill: 'supplier-risk',
        instance_page_id: await freeOrder(api),
        source: 'integration',
        mime: 'text/plain',
        title: 'Who takes me?',
        body: 'Run this task.',
      });
      const runId = (await runStarted(api, trigger.event_id)).run_id!;
      const live = await liveView(api, runId, (v) => v.status === 'running' && !!v.heldBy);
      assert.equal(live.heldBy?.runtime, RUNNER, JSON.stringify(live.heldBy));
      assert.equal(live.heldBy?.pageId, 'markdown/instances/runtime/vsx-controls.md');
      assert.ok(Date.parse(live.heldBy!.until) > Date.now() - 60_000);
      // The lease is released when the run assigns its event: a finished run names no holder.
      const done = await liveView(api, runId, (v) => v.status !== 'running');
      assert.equal(done.heldBy, undefined);
    } finally {
      back();
    }
  });

  test('a skill that requires labels says so on its Knowledge row', async function () {
    this.timeout(60_000);
    const back = signInAsAdmin(api);
    try {
      await api.services.client.updatePage({
        page_id: 'markdown/skills/vsx-routed.md',
        base_sha256: '',
        content: [
          '---',
          'kind: skill',
          'id: vsx-routed',
          'description: Work only the escurel repo runner takes.',
          'autonomy: auto',
          'requires:',
          '  harness: echo',
          '  labels: [repo:escurel]',
          '---',
          '# vsx-routed',
          '',
        ].join('\n'),
      });
    } finally {
      back();
    }
    const skill = await until(
      async () => (await api.services.client.listSkills()).find((s) => s.id === 'vsx-routed'),
      30_000,
      'the routed skill',
    );
    assert.deepEqual(skill.requires, { harness: 'echo', labels: ['repo:escurel'] });
    assert.deepEqual(chipsForSkill(skill), ['auto', 'needs echo · repo:escurel']);
  });
});
