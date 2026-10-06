import { expect, test, webviewWith } from './fixtures';
import { openRow, pane } from './helpers';

/**
 * The Evolve demo scenarios, in a real window: two scripted searches (no model spend, synthetic
 * data) have finished and each has an owner-created comparison page. The owner clicks Compute
 * comparison; Evolve verifies the gateway-attested click, replays both programs and fills the page
 * in; the Scenarios view then reads Evolve's immutable record and opens a native diff.
 *
 * Needs EVOLVE_AGENT_BIN (an evolve-agent built with `--features synthetic-brain`), and
 * ANOFOX_EXTENSION_DIR pointing at a DuckDB 1.5.6 extension profile.
 */
test.use({ evolveAgentBin: process.env.EVOLVE_AGENT_BIN });
test.skip(!process.env.EVOLVE_AGENT_BIN, 'Set EVOLVE_AGENT_BIN to test the Evolve scenarios');

type Played = { scenario: string; experiment: string; comparison: string; page: string };

test('the owner computes a scenario comparison and reads it in the Scenarios view', async ({
  stack,
}) => {
  test.setTimeout(240_000);
  const demo = (await import('../../demo/evolve-scenarios.mjs')) as {
    playScenarios: (args: {
      evolveCall: typeof stack.evolveCall;
      gatewayCall: (
        name: string,
        args: Record<string, unknown>,
      ) => Promise<Record<string, unknown>>;
      owner: string;
    }) => Promise<Played[]>;
  };
  // The assertions below name the P5 (assortment) numbers, so the scenario list is the demo's own.
  const played = await demo.playScenarios({
    evolveCall: stack.evolveCall,
    gatewayCall: (name, args) => stack.call(name, args),
    owner: 'alice',
  });
  expect(played.map((p) => p.scenario)).toEqual(['bin-packing', 'assortment']);

  const assortment = played.find((p) => p.scenario === 'assortment')!;
  const pageContent = async () =>
    String((await stack.call('expand', { page_id: assortment.page, raw: true })).content ?? '');

  // The request page offers the action and nothing else has been computed yet.
  expect(await pageContent()).toContain('status: requested');
  await openRow(stack.page, 'evolve_comparison', new RegExp(assortment.comparison));
  const pageUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await pageUi.getByRole('button', { name: 'Compute comparison', exact: true }).click();
  await stack.shot('evolve-scenarios-requested');

  // Evolve fills the same page in; the click can never be applied twice.
  await expect
    .poll(async () => (await pageContent()).includes('status: completed'), {
      timeout: 90_000,
    })
    .toBe(true);
  const page = await pageContent();
  // The click opened the event thread; open the page itself to see what Evolve filled in.
  await openRow(stack.page, 'evolve_comparison', new RegExp(assortment.comparison));
  // The open editor reloads when it becomes active: it must show what Evolve filled in, and no
  // longer offer a click the gateway would refuse.
  const donePage = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(donePage.getByText('search_time_training_replay')).toBeVisible({ timeout: 20_000 });
  await expect(
    donePage.getByRole('button', { name: 'Compute comparison', exact: true }),
  ).toHaveCount(0);
  await stack.shot('evolve-scenarios-completed-page');
  expect(page).toContain('evidence_scope: search_time_training_replay');
  expect(page).toContain('baseline: parent');
  expect(page).not.toContain('next_comparison_action');

  // Evolve's record is the result; the page carries its hash, and the Household story is in it.
  const record = await stack.evolveCall('evolve_comparison', { comparison: assortment.comparison });
  expect(record.state).toBe('completed');
  expect(page).toContain(String(record.result_sha256));
  const listedAgain = (record.rows as { column_name: string; new_value: string }[]).filter(
    (row) => row.column_name === 'listed' && row.new_value === 'true',
  );
  expect(listedAgain.length).toBeGreaterThan(0);

  // The Scenarios view lists the comparison, shows its table, and opens a native diff.
  const scenarios = pane(stack.page, 'Scenarios');
  // The Compute click reached the view as a live event, and the view keeps looking while the
  // comparison is waiting; nobody has to press Refresh.
  const row = scenarios.getByRole('treeitem', { name: new RegExp(assortment.comparison) });
  await expect(row).toContainText('completed', { timeout: 30_000 });
  await row.click();
  const table = scenarios.getByRole('treeitem', { name: /p5_state/ });
  await expect(table).toContainText('modified');
  await table.click();
  await expect(stack.page.getByRole('tab', { name: /baseline ↔ candidate/ })).toBeVisible();
  // The diff is not blank: the candidate side lists products the winner put back on the shelf.
  await expect(stack.page.locator('.monaco-diff-editor')).toContainText('listed = true', {
    timeout: 20_000,
  });
  await stack.shot('evolve-scenarios-diff');

  // The bin-packing scenario, computed the same way, with its own diff.
  const packing = played.find((p) => p.scenario === 'bin-packing')!;
  await openRow(stack.page, 'evolve_comparison', new RegExp(packing.comparison));
  const packingUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await packingUi.getByRole('button', { name: 'Compute comparison', exact: true }).click();
  await expect(
    scenarios.getByRole('treeitem', { name: new RegExp(packing.comparison) }),
  ).toContainText('completed', { timeout: 60_000 });
  await scenarios.getByRole('treeitem', { name: new RegExp(packing.comparison) }).click();
  await scenarios.getByRole('treeitem', { name: /p0_bin_assignment/ }).click();
  await expect(
    stack.page.getByRole('tab', { name: /p0_bin_assignment: baseline ↔ candidate/ }),
  ).toBeVisible();
  await stack.shot('evolve-scenarios-bin-packing-diff');

  // A page that only claims to be completed is not a result: the view shows nothing from Evolve.
  const forged = 'markdown/instances/evolve_comparison/forged.md';
  const written = await stack.call('update_page', {
    page_id: forged,
    content: [
      '---',
      'kind: instance',
      'skill: evolve_comparison',
      'id: forged',
      'owner_subject: "alice"',
      `experiment: ${assortment.experiment}`,
      'baseline: seed',
      'candidate: winner',
      'status: completed',
      `result_sha256: "${'0'.repeat(64)}"`,
      '---',
      '',
      '# Forged',
      '',
    ].join('\n'),
    base_sha256: '',
  });
  expect(written.ok).toBe(true);
  // The title actions only appear while the pane header is hovered.
  await scenarios.locator('.pane-header').hover();
  await scenarios.getByRole('button', { name: /Refresh scenario/ }).click();
  const forgedRow = scenarios.getByRole('treeitem', { name: /forged/ });
  await forgedRow.click();
  await expect(scenarios.getByRole('treeitem', { name: /Unverified/ })).toBeVisible();
  // VS Code's own extension host prints a Node deprecation warning; anything else is ours.
  expect(
    stack.errors.filter(
      (e) => !/favicon|ResizeObserver|DeprecationWarning|trace-deprecation/.test(e),
    ),
  ).toEqual([]);
});
