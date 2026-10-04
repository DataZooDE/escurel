import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { Event } from '../../../src/client/types';
import type { EscurelApi } from '../../../src/extension';
import { buildControlEvent } from '../../../src/runs/controls';
import { matchResult } from '../../../src/runs/controlResult';
import { activate, signInAsAdmin, until } from '../cascade/support';
import { requireEnv } from '../requireEnv';

const page = 'markdown/instances/customer-order__order-4500123.md';
const results = (api: EscurelApi) =>
  api.services.client.listEvents({
    label_skill: 'escurel:run-control-result',
    include_system: true,
    newest_first: true,
    limit: 20,
  });

async function finished(api: EscurelApi, eventId: string, status: string): Promise<Event> {
  return until(
    async () => {
      const rows = await api.services.client.listEvents({
        label_skill: 'escurel:run',
        include_system: true,
        newest_first: true,
        limit: 100,
      });
      return rows.events.find(
        (e) =>
          e.title === 'run-finished' &&
          e.provenance?.runner &&
          (e.provenance.runner as Record<string, unknown>).event_id === eventId &&
          JSON.parse(e.body ?? '{}').status === status,
      );
    },
    90_000,
    `run to finish as ${status}`,
  );
}

suite('real run controls', () => {
  let api: EscurelApi;
  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
  });

  test('cancel a sleeping run and retry it as a new attempt', async function () {
    this.timeout(180_000);
    const trigger = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      instance_page_id: page,
      source: 'integration',
      mime: 'text/plain',
      title: 'Control test',
      body: 'Run this task.',
    });
    const started = await until(
      async () => {
        const rows = await api.services.client.listEvents({
          label_skill: 'escurel:run',
          include_system: true,
          newest_first: true,
          limit: 100,
        });
        return rows.events.find(
          (e) =>
            e.title === 'run-started' &&
            (e.provenance?.runner as Record<string, unknown> | undefined)?.event_id ===
              trigger.event_id,
        );
      },
      60_000,
      'run-started',
    );
    assert.ok(started.run_id);
    await vscode.commands.executeCommand('escurel.cancelRun', started.run_id);
    await finished(api, trigger.event_id, 'cancelled');
    await vscode.commands.executeCommand('escurel.retryRun', started.run_id);
    const retry = await until(
      async () => {
        const rows = await results(api);
        return rows.events
          .map((e) => JSON.parse(e.body ?? '{}'))
          .find(
            (body) =>
              body.action === 'retry' &&
              body.run_id === started.run_id &&
              body.outcome === 'requeued',
          );
      },
      30_000,
      'retry result',
    );
    assert.ok(retry.new_run_id);
    assert.notEqual(retry.new_run_id, started.run_id);
  });

  test('admin requeues a dead letter, and the human is denied', async function () {
    this.timeout(180_000);
    const trigger = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      instance_page_id: page,
      source: 'workbench',
      mime: 'text/plain',
      title: 'Dead letter test',
      body: 'Run this task.',
      provenance: { manual: { mode: 'run', harness: 'no-such-harness' } },
    });
    await finished(api, trigger.event_id, 'dead_letter');
    assert.equal(await api.services.admin.get(), 'not-admin');
    await until(() => (api.canAdmin() === false ? true : undefined), 5000, 'human context');
    await assert.rejects(
      () =>
        api.services.client.captureEvent(
          buildControlEvent({ action: 'requeue', eventId: trigger.event_id }),
        ),
      /no such run, or not yours to control/,
    );
    const back = signInAsAdmin(api);
    try {
      assert.equal(await api.services.admin.get(), 'admin');
      await until(() => (api.canAdmin() === true ? true : undefined), 5000, 'admin context');
      await vscode.commands.executeCommand('escurel.requeue', trigger.event_id);
      // THIS requeue: the control event naming our dead-lettered trigger, and the runner's answer to
      // that very request. "Some requeue succeeded at some point" would pass on another test's.
      const requests = await api.services.client.listEvents({
        label_skill: 'escurel:run-control',
        include_system: true,
        newest_first: true,
        limit: 20,
      });
      const mine = requests.events.find((e) => {
        try {
          return JSON.parse(e.body ?? '{}').event_id === trigger.event_id;
        } catch {
          return false;
        }
      });
      assert.ok(mine, 'the requeue request for this dead letter was captured');
      const answer = await until(
        async () =>
          matchResult((await results(api)).events, { eventId: mine.event_id, action: 'requeue' }),
        30_000,
        'the runner’s answer to this requeue',
      );
      assert.equal(answer.outcome, 'requeued');
    } finally {
      back();
    }
  });

  test('admin pauses and resumes dispatch', async function () {
    this.timeout(120_000);
    const back = signInAsAdmin(api);
    // A failed assertion must not leave the tenant paused: every later suite would then time out
    // waiting for a runner that is deliberately not dispatching.
    let paused = false;
    try {
      // The modal is a real user confirmation; capture directly to verify the same gateway and runner path.
      const pause = await api.services.client.captureEvent(buildControlEvent({ action: 'pause' }));
      paused = true;
      const pausedResult = await until(
        async () =>
          matchResult((await results(api)).events, { eventId: pause.event_id, action: 'pause' }),
        30_000,
        'pause result',
      );
      assert.equal(pausedResult.outcome, 'paused');
      const resume = await api.services.client.captureEvent(
        buildControlEvent({ action: 'resume' }),
      );
      paused = false;
      const resumed = await until(
        async () =>
          matchResult((await results(api)).events, { eventId: resume.event_id, action: 'resume' }),
        30_000,
        'resume result',
      );
      assert.equal(resumed.outcome, 'resumed');
    } finally {
      if (paused) {
        await api.services.client
          .captureEvent(buildControlEvent({ action: 'resume' }))
          .catch(() => undefined);
      }
      back();
    }
  });
});
