import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { RunsNode } from '../../../src/views/runsModel';
import { activate, discardOpenDrafts, freeOrder, until } from './support';
import { requireEnv } from '../requireEnv';

const group = (roots: RunsNode[], id: string) => roots.find((n) => n.id === id);
const runs = (roots: RunsNode[], id: string) =>
  (group(roots, id)?.children ?? []).filter((c) => c.kind === 'run');

suite('runs panel in cascade', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
  });

  teardown(async () => {
    if (api) await discardOpenDrafts(api);
  });

  suiteTeardown(async () => {
    await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    if (api) await discardOpenDrafts(api);
  });

  test('the panel says in words that dispatch is on and what the runner is', async function () {
    this.timeout(120_000);
    const dispatch = await until(
      async () => (await api.runner.getChildren()).find((n) => n.id === 'dispatch'),
      60_000,
      'the dispatch row',
    );
    assert.equal(dispatch.label, 'Dispatch is on');
    assert.equal(dispatch.contextValue, 'dispatch.running');
    // The runner sentence is the view's own message, with the harness in words, not "harness: echo".
    const sentence = await until(
      () => {
        const m = api.runner.viewMessage;
        return /Runner ok · echo harness \(demo, no AI model\) · last seen/.test(m) ? m : undefined;
      },
      60_000,
      'the runner sentence',
    );
    assert.ok(sentence);
  });

  test('a run that finishes shows up in History as succeeded, with no refresh by hand', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);
    const e = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Supplier risk for the panel test',
      body: 'Testing the live runs panel.',
      instance_page_id: page,
    });
    const row = await until(
      async () =>
        runs(await api.runner.getChildren(), 'group:history').find((r) => r.eventId === e.event_id),
      90_000,
      'the finished run in History',
    );
    assert.equal(row.state, 'succeeded');
    // It names the skill and the page, never an id.
    assert.match(row.label, /^supplier-risk · order-/);
    assert.doesNotMatch(row.label, /[0-9A-Z]{20,}/);
    assert.match(row.description ?? '', /^ok · /);
    assert.match(row.tooltip ?? '', /succeeded/);
    // And it is not still "running": a stale Running row after a run ended was the old panel's bug.
    const roots = await api.runner.getChildren();
    assert.equal(
      runs(roots, 'group:running').find((r) => r.eventId === e.event_id),
      undefined,
    );
    assert.equal(row.contextValue, 'run.done');
  });

  test('a failed run is under Needs attention with its reason, and can be retried', async function () {
    this.timeout(180_000);
    const page = await freeOrder(api);
    const e = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      mime: 'text/plain',
      source: 'integration',
      title: 'Provoked failure for the panel test',
      body: 'Trigger harness refusal.',
      instance_page_id: page,
      provenance: { manual: { harness: 'no-such-harness' } },
    });
    const row = await until(
      async () =>
        runs(await api.runner.getChildren(), 'group:attention').find(
          (r) => r.eventId === e.event_id,
        ),
      90_000,
      'the failure under Needs attention',
    );
    assert.equal(row.contextValue, 'run.failed');
    assert.ok(row.runId, 'the row carries the run id the retry and open commands read');
    assert.match(row.description ?? '', /^(gave up|failed)/);
    // The reason is its own row beneath, not a cut-off description.
    assert.equal(row.children?.[0]?.kind, 'reason');
    assert.ok(row.children![0]!.label.length > 0);
    assert.match(row.tooltip ?? '', /Reason: /);
    // The same failure is in History too: History is everything that ended.
    const history = runs(await api.runner.getChildren(), 'group:history');
    assert.ok(history.some((r) => r.runId === row.runId));
  });

  test('History pages: 25 at a time, Load more shows older runs, nothing is shown twice', async function () {
    this.timeout(280_000);
    // Real runs, many of them: each is refused at once (no such harness), so they are cheap.
    const page = await freeOrder(api);
    const total = 62;
    for (let i = 0; i < total; i += 1) {
      await api.services.client.captureEvent({
        label_skill: 'supplier-risk',
        mime: 'text/plain',
        source: 'integration',
        title: `paging ${i}`,
        body: 'refused',
        instance_page_id: page,
        provenance: { manual: { harness: 'no-such-harness' } },
      });
    }
    const countAll = async () => {
      const h = group(await api.runner.getChildren(), 'group:history');
      return Number(h?.description ?? 0);
    };
    await until(
      async () => ((await countAll()) >= total ? true : undefined),
      200_000,
      'all runs in History',
    );
    const shown = async () => runs(await api.runner.getChildren(), 'group:history');
    assert.equal((await shown()).length, 25, 'History shows one page');
    const more = group(await api.runner.getChildren(), 'group:history')!.children!.at(-1)!;
    assert.equal(more.kind, 'more');
    await api.runner.loadMore();
    assert.equal((await shown()).length, 50);
    await api.runner.loadMore();
    await api.runner.loadMore();
    const all = await shown();
    assert.ok(all.length >= total, `every run is reachable: ${all.length}`);
    assert.equal(new Set(all.map((r) => r.runId)).size, all.length, 'no run is listed twice');
  });

  test('a filter narrows History and says so when nothing matches', async function () {
    this.timeout(60_000);
    await api.runner.setFilter({ states: ['succeeded'] });
    let h = group(await api.runner.getChildren(), 'group:history')!;
    assert.ok(
      h.children!.every((c) => c.kind === 'run' && c.state === 'succeeded') ||
        h.children![0]!.kind === 'empty',
    );
    await api.runner.setFilter({ text: 'no run mentions this at all' });
    h = group(await api.runner.getChildren(), 'group:history')!;
    assert.equal(h.children![0]!.label, 'No runs match the filter.');
    await api.runner.setFilter({});
  });

  test('"Show failed runs" (the link under Needs attention) filters History to failures', async function () {
    this.timeout(60_000);
    await vscode.commands.executeCommand('escurel.runs.showFailed');
    const h = group(await api.runner.getChildren(), 'group:history')!;
    const rows = h.children!.filter((c) => c.kind === 'run');
    assert.ok(rows.length > 0, 'the earlier tests left failures');
    assert.ok(rows.every((r) => r.state === 'failed' || r.state === 'dead_letter'));
    assert.match(api.runner.viewMessage, /Filtered: failed/);
    await vscode.commands.executeCommand('escurel.runs.clearFilter');
    assert.doesNotMatch(api.runner.viewMessage, /Filtered:/);
  });
});
