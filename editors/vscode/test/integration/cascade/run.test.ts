// A REAL run, driven by a real runner and the real echo harness against a real
// gateway. No model, no mocks. This is the fixture M3's Thread view and run
// detail stand on, so it is proved before anything is built on it.
//
// It runs against its OWN gateway (see index.ts) whose inbox starts empty,
// because the echo harness folds the oldest inbox event carrying a target
// instance rather than the trigger its run was dispatched for.
//
// WHAT THIS HARNESS CANNOT PROVE, and why — decided with the owner rather than
// discovered later:
//
// The runner here carries a STATIC bearer, because the gateway runs without a
// verifier. A static bearer carries no run claims, so the gateway stamps no
// `run_id` on the agent's draft (#510: only the token can prove which run wrote
// something, and that is deliberate — nothing a caller sends could). Three
// things follow, all verified against a live gateway before this comment was
// written:
//
//   * `list_lineage` shows the event and the run, never the draft or changeset,
//     because a draft with no `run_id` cannot be folded under its run;
//   * promoting that draft produces NO cascade event at all, so E2a-c never
//     arrives (30 s of polling, nothing);
//   * `get_run_tool_calls` is empty and no plan exists, because per-call rows
//     are attributed by the run-bound token too, and the echo harness reports no
//     plan snapshots.
//
// A lineage-complete cascade needs the runner minting per-run tokens, which
// needs a verifying gateway with an issuer in this harness. That is written down
// in the M3 plan as a deliberate gap: folding a changeset under a run, pruned
// subtrees, cascade events and plan step states are covered by recorded lineage
// fixtures in the unit and component suites instead.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

const SKILL = 'supplier-risk';
// Three orders, as the mock draws them. A page carries at most one open draft, so
// a suite that shared one page would have its tests contend for the only slot —
// and a run whose `create_draft` is refused dead-letters, which reads exactly like
// a runner that never started.
const PAGES = [
  'markdown/instances/supplier-risk__order-4500123.md',
  'markdown/instances/supplier-risk__order-4500124.md',
  'markdown/instances/supplier-risk__order-4500131.md',
];

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until<T>(f: () => Promise<T | undefined> | T | undefined, ms = 45_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined) return v;
    if (Date.now() > end) throw new Error('timed out');
    await wait(400);
  }
}

suite('a real run', () => {
  let api: EscurelApi;

  suiteSetup(async function () {
    this.timeout(120_000);
    if (!process.env.ESCUREL_TEST_RUNNER) this.skip();
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    api = (await ext.activate()) as EscurelApi;
  });

  /** An order with no open draft, so this test owns the page's only draft slot. */
  async function freePage(): Promise<string> {
    const open = (await api.services.client.listDrafts()).filter((d) => d.status === 'open');
    const taken = new Set(open.map((d) => d.target_page_id));
    const free = PAGES.find((p) => !taken.has(p));
    assert.ok(free, `every order already carries an open draft: ${[...taken].join(', ')}`);
    return free;
  }

  test('a captured signal becomes a run in the lineage, and holds a draft', async function () {
    this.timeout(120_000);
    const PAGE = await freePage();

    // E1: the trigger. The runner picks it up by `label_skill`.
    const captured = await api.services.client.captureEvent({
      label_skill: SKILL,
      mime: 'text/plain',
      source: 'integration',
      body: 'Supplier Meier-Guss flagged: delivery risk raised to high, ETA slips 3 weeks.',
      instance_page_id: PAGE,
    });
    const rootEventId = captured.event_id;
    assert.ok(rootEventId, 'the trigger must have an id to root the lineage');

    // The run, read through `list_lineage` because that is the shape the Thread
    // view consumes. A timeout reports the lineage it last saw: "timed out"
    // alone cannot tell a runner that never started from a run that failed.
    let lastSeen = 'nothing';
    const run = await until(async () => {
      const lineage = await api.services.client.listLineage({
        root_event_id: rootEventId,
        include: ['events', 'runs', 'drafts', 'tool_calls'],
      });
      lastSeen = lineage.nodes.map((n) => `${n.type}:${n.state}`).join(' ') || 'no nodes';
      return lineage.nodes.find((n) => n.type === 'run' && n.state !== 'running');
    }).catch((e: unknown) => {
      throw new Error(`no finished run under ${rootEventId} (last saw: ${lastSeen}): ${String(e)}`);
    });

    // The tree's shape: a run's parent is the event that triggered it.
    assert.equal(run.parent, rootEventId);
    assert.equal(run.state, 'processed', `the run must succeed: ${JSON.stringify(run)}`);

    // What run detail reads off this node. Asserted because a run node with no
    // attributes would still satisfy "a run exists", and the view would have
    // nothing to show.
    assert.equal(run.harness, 'echo');
    assert.equal(run.autonomy, 'review');
    assert.equal(run.target_page_id, PAGE);
    assert.equal(run.held, true, 'a review run holds its write');
    assert.equal(run.attempts, 1);
    assert.ok(typeof run.trace_id === 'string' && run.trace_id.length > 0, 'a copyable trace id');
    assert.ok(typeof run.started_at === 'string', 'an attempts timeline needs a start');
    assert.ok(typeof run.finished_at === 'string', '…and an end');
    assert.ok(
      typeof run.summary === 'string' && run.summary.includes('awaiting a human'),
      `the run says what it did: ${String(run.summary)}`,
    );

    // And the write really is held rather than landed: the draft exists and the
    // page has not moved.
    const draft = await until(async () => {
      const drafts = await api.services.client.listDrafts();
      return drafts.find((d) => d.status === 'open' && d.target_page_id === PAGE);
    });
    assert.equal(draft.author.length > 0, true, 'the draft records who proposed it');
    const page = await api.services.client.expand({ page_id: PAGE, raw: true });
    assert.ok(
      !page.content?.includes(String(captured.event_id)),
      'the page must not carry the fold until a human promotes it',
    );
  });
});
