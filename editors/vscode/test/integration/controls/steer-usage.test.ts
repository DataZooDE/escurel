import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { Event } from '../../../src/client/types';
import type { RunView } from '../../../src/shared/protocol';
import type { EscurelApi } from '../../../src/extension';
import {
  activate,
  discardOpenDrafts,
  freeOrder,
  settleInbox,
  signInAsAdmin,
  until,
} from '../cascade/support';
import { requireEnv } from '../requireEnv';
import { chipsForSkill } from '../../../src/views/knowledgeModel';

// A real gateway that verifies tokens, a real runner minting per-run tokens and the real echo
// harness, which sleeps (ESCUREL_ECHO_SLEEP_MS) so a message can reach the run while it is live,
// folds every injected turn into the draft it writes (`(steered: …)`) and prints the scripted
// transcript the harness runner gives it (ESCUREL_ECHO_TRANSCRIPT, see runTests.ts).
const STEER = 'use the 2027 terms, not 2026';

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

/** Open run detail and wait for the host to load a view of the run that `accept`s. */
function viewFor(
  api: EscurelApi,
  runId: string,
  accept: (view: RunView) => boolean,
  ms = 60_000,
): Promise<RunView> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose();
      reject(new Error('run detail did not show the expected state'));
    }, ms);
    const sub = api.runs.onDidLoad(({ runId: id, view }) => {
      if (id !== runId || !accept(view)) return;
      clearTimeout(timer);
      sub.dispose();
      resolve(view);
    });
    void vscode.commands.executeCommand('escurel.openRun', runId);
  });
}

suite('steering, transcript and usage against the real stack', () => {
  let api: EscurelApi;
  suiteSetup(async function () {
    this.timeout(120_000);
    requireEnv(this, 'ESCUREL_TEST_RUNNER');
    api = await activate();
    await discardOpenDrafts(api);
  });
  teardown(async function () {
    this.timeout(60_000);
    // A draft left open on an order makes the next run on it dead-letter.
    await discardOpenDrafts(api);
  });

  test('a steer reaches a live echo run, shows in its record and in what it wrote; the transcript replays', async function () {
    this.timeout(240_000);
    const page = await freeOrder(api);
    const trigger = await api.services.client.captureEvent({
      label_skill: 'supplier-risk',
      instance_page_id: page,
      source: 'integration',
      mime: 'text/plain',
      title: 'Steer me',
      body: 'Run this task.',
    });
    const started = await runStarted(api, trigger.event_id);
    const runId = started.run_id!;
    // The offer comes from the runtime holding the run (#827), once its page and the claim are read.
    const live = await viewFor(
      api,
      runId,
      (v) => v.status === 'running' && !!v.steer && !(v.steer.available && v.steer.hint),
    );
    assert.deepEqual(live.steer, { available: true }, 'the runtime says an echo run takes messages');

    // A message the gateway would refuse is refused before anything is sent.
    assert.deepEqual(await api.runs.steer(runId, '   '), {
      sent: false,
      message: 'Write a message first.',
    });

    const reply = await api.runs.steer(runId, STEER);
    assert.deepEqual(reply, {
      sent: true,
      message: 'Message sent. The agent reads it as its next turn.',
    });

    // The run's own record lists it, and so does run detail; the finished run names its transcript.
    // (One wait: the panel is already open, so a second `openRun` only reveals it, loading nothing.)
    const finished = await viewFor(
      api,
      runId,
      (v) =>
        v.status !== 'running' &&
        v.transcript !== undefined &&
        v.injections.some((m) => m.text === STEER),
      120_000,
    );
    assert.equal(finished.injections[0]?.n, 1);
    assert.equal(finished.steer, undefined, 'a finished run offers no steer box');

    // The agent read it: the fold it drafted carries the message.
    const draft = await until(
      async () =>
        (await api.services.client.listDrafts()).find(
          (d) => d.run_id === runId && d.content.includes(`steered: ${STEER}`),
        ),
      60_000,
      'the steered draft',
    );
    assert.equal(draft.target_page_id, page);

    // Too late now: the runner answers not_live, and the box says so instead of pretending.
    assert.equal((await api.runs.steer(runId, 'too late')).sent, false);

    // The transcript the echo harness wrote is replayed through the host's own read.
    assert.ok(finished.transcript && finished.transcript.records > 0);
    const transcript = await api.runs.transcript(runId);
    assert.equal(transcript.status, 'ready', JSON.stringify(transcript));
    if (transcript.status !== 'ready') return;
    assert.deepEqual(
      transcript.records.map((r) => r.kind),
      ['message', 'tool_call', 'tool_result'],
    );
    assert.equal(transcript.records[1]?.name, 'expand');
  });

  test('usage needs an admin; an admin sees runs, success and cost by skill and by agent', async function () {
    this.timeout(120_000);
    await api.usage.load();
    assert.equal(api.usage.message, 'Usage needs an admin sign-in.');
    assert.deepEqual(api.usage.rows(), []);

    const back = signInAsAdmin(api);
    try {
      await api.usage.setWindow('24h');
      const total = api.usage.rows()[0];
      assert.equal(total?.label, 'All runs', api.usage.message);
      assert.match(total.description, /^\d+ runs? · \d+% ok · /);
      assert.equal(api.usage.message, 'Last 24 hours, by skill');
      assert.ok(
        api.usage.rows().some((r) => r.label === 'supplier-risk'),
        JSON.stringify(api.usage.rows()),
      );
      await api.usage.setGroupBy('agent');
      assert.equal(api.usage.message, 'Last 24 hours, by agent');
      // Without a profile a run acts as `agent:<skill>`.
      assert.ok(api.usage.rows().some((r) => r.label === 'supplier-risk'));
    } finally {
      await api.usage.setGroupBy('skill').catch(() => undefined);
      back();
    }
  });

  test('the Agents view lists a profile an admin wrote, with its engine and scopes', async function () {
    this.timeout(120_000);
    const back = signInAsAdmin(api);
    try {
      await api.services.client.updatePage({
        page_id: 'markdown/instances/agent/vsx-scribe.md',
        base_sha256: '',
        content: [
          '---',
          'kind: instance',
          'skill: agent',
          'id: vsx-scribe',
          'harness: echo',
          'skills: ["[[skill::supplier-risk]]"]',
          'scopes:',
          '  tools: [expand, create_draft]',
          '  write: ["markdown/instances/customer-order__*"]',
          'max_concurrent: 1',
          '---',
          '# vsx-scribe',
          '',
        ].join('\n'),
      });
      await api.agents.load();
      const row = api.agents.rows().find((r) => r.label === 'vsx-scribe');
      assert.ok(row, JSON.stringify(api.agents.rows()));
      assert.equal(row.description, 'echo · no runs');
      assert.match(row.tooltip, /Tools: expand, create_draft/);
      assert.equal(row.pageId, 'markdown/instances/agent/vsx-scribe.md');
    } finally {
      back();
    }
    // A member reads the profiles too (the agent skill is public to read), without the counts.
    await api.agents.load();
    assert.ok(api.agents.rows().some((r) => r.label === 'vsx-scribe'));
    assert.equal(api.agents.message, 'Run counts need an admin sign-in.');
  });

  test('a skill whose daily budget is spent shows its skipped window in the Runs view, and its schedule where skills are listed', async function () {
    this.timeout(240_000);
    const pageId = 'markdown/skills/vsx-digest.md';
    const skillPage = (schedule: string) =>
      [
        '---',
        'kind: skill',
        'id: vsx-digest',
        'description: A digest the runner starts on its own.',
        'autonomy: auto',
        schedule,
        '---',
        '# vsx-digest',
        '',
      ].join('\n');
    const back = signInAsAdmin(api);
    try {
      // Every minute, at most once a day: the first window fires, the next one is skipped and recorded.
      await api.services.client.updatePage({
        page_id: pageId,
        base_sha256: '',
        content: skillPage('schedule: "* * * * *"\nschedule_budget: {runs_per_day: 1}'),
      });
      const listed = (await api.services.client.listSkills()).find((s) => s.id === 'vsx-digest');
      assert.equal(listed?.schedule, '* * * * *');
      assert.deepEqual(listed?.schedule_budget, { runs_per_day: 1 });
      const chips = chipsForSkill(listed!, new Date());
      assert.match(chips.at(-1) ?? '', /^scheduled · next \d\d:\d\d UTC$/);

      await until(
        async () => {
          await api.runner.refresh();
          const roots = await api.runner.getChildren();
          const group = roots.find((r) => r.id === 'group:skipped');
          return group?.children?.find((c) => c.label === 'Skipped · vsx-digest');
        },
        180_000,
        'a skipped window in the Runs view',
      );
    } finally {
      // Stop the schedule before anything else runs against this runner.
      const head = await api.services.client.expand({ page_id: pageId }).catch(() => undefined);
      await api.services.client
        .updatePage({
          page_id: pageId,
          base_sha256: head?.content_sha256 ?? '',
          content: skillPage('schedule_budget: {runs_per_day: 1}').replace(
            'schedule_budget: {runs_per_day: 1}\n',
            '',
          ),
        })
        .catch(() => undefined);
      back();
      await settleInbox(api);
    }
  });
});
