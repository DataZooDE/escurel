import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import { activate, discardOpenDrafts, freeOrder, until } from './support';

suite('runner view in cascade', () => {
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

  test('the tree shows a runner row with the harness echo', async function () {
    this.timeout(120_000);
    const runnerRow = await until(
      async () => {
        const rows = await api.runner.getChildren();
        const r = rows.find((row) => row.kind === 'runner');
        return r?.description?.includes('echo') ? r : undefined;
      },
      60_000,
      'runner row with harness echo',
    );

    assert.ok(runnerRow, 'expected runner row to be present');
    assert.ok(runnerRow.description?.includes('echo'), 'expected harness echo in description');
  });

  test('starting a run makes the Runs row show it live then processed without manual refresh', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);

    // Initial processed count
    const initialRows = await api.runner.getChildren();
    const initialRunsRow = initialRows.find((r) => r.kind === 'runs');
    const initialProcessedMatch = initialRunsRow?.description?.match(/(\d+)\s+processed/);
    const initialProcessed = initialProcessedMatch
      ? parseInt(initialProcessedMatch[1] ?? '0', 10)
      : 0;

    // Start a run by capturing an event on a free order page
    const e1 = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Supplier risk for runner test',
      body: 'Testing live runner updates.',
      instance_page_id: page,
    });
    assert.ok(e1.event_id);

    // Without a manual refresh, live runner status arrives via WebSocket and updates Runs row
    await until(
      async () => {
        const rows = await api.runner.getChildren();
        const runsRow = rows.find((r) => r.kind === 'runs');
        const processedMatch = runsRow?.description?.match(/(\d+)\s+processed/);
        const processed = processedMatch ? parseInt(processedMatch[1] ?? '0', 10) : 0;
        return processed > initialProcessed ? true : undefined;
      },
      90_000,
      'processed count to increment in Runs row',
    );

    const updatedRows = await api.runner.getChildren();
    const updatedRunsRow = updatedRows.find((r) => r.kind === 'runs');
    assert.ok(updatedRunsRow, 'Runs row should be present');
  });

  test('a provoked dead letter shows under Dead letters with its reason', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);

    // Provoke a dead letter by requesting a non-existent harness
    const dlEvent = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Provoked dead letter',
      body: 'Trigger harness refusal.',
      instance_page_id: page,
      provenance: {
        manual: {
          harness: 'no-such-harness',
        },
      },
    });
    assert.ok(dlEvent.event_id);

    // Wait until Dead letters group shows the dead letter with its refusal reason
    const deadLetterRow = await until(
      async () => {
        const rows = await api.runner.getChildren();
        const deadLettersGroup = rows.find((r) => r.kind === 'deadLetters');
        if (!deadLettersGroup || !deadLettersGroup.children) return undefined;
        return deadLettersGroup.children.find(
          (c) =>
            c.description?.includes('permanent') ||
            c.description?.includes('refusing') ||
            c.description?.includes('no-such-harness'),
        );
      },
      90_000,
      'dead letter to show in runner tree under Dead letters',
    );

    assert.ok(deadLetterRow, 'dead letter row should appear');
    assert.equal(deadLetterRow.contextValue, 'deadLetter');
    assert.ok(deadLetterRow.runId, 'dead letter row should carry runId');
  });
});
