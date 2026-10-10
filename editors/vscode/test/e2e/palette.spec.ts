import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from './fixtures';
import { filterCommandPalette } from './helpers';

// The command palette lists the Evolve intake commands in a REAL window: evolve-service.spec.ts (the joined
// Evolve service journey, run by the Evolve repo's CI) types `>Prepare private Anofox Evolve training CSV`
// into the palette right after opening a CSV, and failed once with the UNFILTERED command list on screen.
// This spec pins the part of that journey that belongs to this repo: the commands are contributed, are
// listed whatever the endpoint setting, and the palette filter applies after a file was opened by quick open.
test('the Evolve intake commands are listed in the command palette after a CSV was opened', async ({
  stack,
}) => {
  const { page } = stack;
  const csv = join(stack.workspaceDir, 'training-demand.csv');
  writeFileSync(csv, 'sku_id,date,true_demand\n1,2026-07-31,1\n');

  await page.keyboard.press('Control+P');
  const quickInput = page.locator('.quick-input-widget input');
  await quickInput.fill(csv);
  await expect(page.locator('.quick-input-list')).toContainText('training-demand.csv');
  await quickInput.press('Enter');
  await expect(page.getByRole('tab', { name: 'training-demand.csv' })).toBeVisible();

  await filterCommandPalette(
    page,
    'Prepare private Anofox Evolve training CSV',
    'Prepare private Anofox Evolve training CSV',
  );
  await stack.shot('30-palette-evolve-csv');
});
