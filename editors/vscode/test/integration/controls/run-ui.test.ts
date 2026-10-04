import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { Event } from '../../../src/client/types';
import type { RunView } from '../../../src/shared/protocol';
import type { EscurelApi } from '../../../src/extension';
import { activate, signInAsAdmin, until } from '../cascade/support';
import { requireEnv } from '../requireEnv';

const page = 'markdown/instances/customer-order__order-4500123.md';

async function runStarted(api: EscurelApi, eventId: string): Promise<Event> {
  return until(
    async () => {
      const rows = await api.services.client.listEvents({
        label_skill: 'escurel:run',
        include_system: true,
        newest_first: true,
        limit: 100,
      });
      return rows.events.find(
        (event) =>
          event.title === 'run-started' &&
          (event.provenance?.runner as Record<string, unknown> | undefined)?.event_id === eventId,
      );
    },
    60_000,
    'run-started',
  );
}

async function viewFor(
  api: EscurelApi,
  runId: string,
  accept: (view: RunView) => boolean,
): Promise<RunView> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error('run detail did not load'));
    }, 30_000);
    const sub = api.runs.onDidLoad(({ runId: id, view }) => {
      if (id !== runId || !accept(view)) return;
      clearTimeout(timer);
      sub.dispose();
      resolve(view);
    });
    void vscode.commands.executeCommand('escurel.openRun', runId);
  });
}

suite('run detail control host path', () => {
  let api: EscurelApi;
  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
  });

  test('a sleeping run offers Cancel; a forged run id has no effect; Cancel ends it', async function () {
    this.timeout(180_000);
    const trigger = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      instance_page_id: page,
      source: 'integration',
      mime: 'text/plain',
      title: 'Run UI cancel',
      body: 'Run this task.',
    });
    const started = await runStarted(api, trigger.event_id);
    assert.ok(started.run_id);
    const view = await viewFor(
      api,
      started.run_id,
      (v) => v.controls?.some((c) => c.action === 'cancel') ?? false,
    );
    assert.ok(view.controls?.some((c) => c.action === 'cancel' && c.enabled));
    const beforeForgery = await api.services.client.listEvents({
      label_skill: 'escurel:run-control',
      newest_first: true,
      limit: 50,
    });
    await api.runs.handleWebviewMessage(started.run_id, {
      type: 'run-control',
      action: 'cancel',
      runId: 'another-run',
    });
    const afterForgery = await api.services.client.listEvents({
      label_skill: 'escurel:run-control',
      newest_first: true,
      limit: 50,
    });
    assert.deepEqual(
      afterForgery.events.map((event) => event.event_id),
      beforeForgery.events.map((event) => event.event_id),
    );
    await api.runs.handleWebviewMessage(started.run_id, {
      type: 'run-control',
      action: 'cancel',
      runId: started.run_id,
    });
    await until(
      async () => {
        const rows = await api.services.client.listEvents({
          label_skill: 'escurel:run',
          include_system: true,
          newest_first: true,
          limit: 100,
        });
        return rows.events.find(
          (event) =>
            event.title === 'run-finished' &&
            event.run_id === started.run_id &&
            JSON.parse(event.body ?? '{}').status === 'cancelled',
        );
      },
      90_000,
      'cancelled run',
    );
  });

  test('dead letter keeps Requeue visible but disabled for a human and enables it for an admin', async function () {
    this.timeout(180_000);
    const trigger = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      instance_page_id: page,
      source: 'workbench',
      mime: 'text/plain',
      title: 'Run UI dead letter',
      body: 'Run this task.',
      provenance: { manual: { mode: 'run', harness: 'no-such-harness' } },
    });
    const started = await runStarted(api, trigger.event_id);
    assert.ok(started.run_id);
    await until(
      async () => {
        const rows = await api.services.client.listEvents({
          label_skill: 'escurel:run',
          include_system: true,
          newest_first: true,
          limit: 100,
        });
        return rows.events.find(
          (event) =>
            event.title === 'run-finished' &&
            event.run_id === started.run_id &&
            JSON.parse(event.body ?? '{}').status === 'dead_letter',
        );
      },
      90_000,
      'dead letter',
    );
    const human = await viewFor(api, started.run_id, (v) => v.status === 'dead_letter');
    assert.equal(human.controls?.find((c) => c.action === 'requeue')?.enabled, false);
    assert.ok(human.controls?.find((c) => c.action === 'requeue')?.disabledReason);
    const back = signInAsAdmin(api);
    try {
      const admin = await viewFor(
        api,
        started.run_id,
        (v) => v.controls?.find((c) => c.action === 'requeue')?.enabled === true,
      );
      assert.equal(admin.controls?.find((c) => c.action === 'requeue')?.enabled, true);
    } finally {
      back();
    }
  });
});
