// The Knowledge tree against a REAL gateway: skills grouped by their `folder:`, iconed by `role:`,
// sorted by role, with the accessible name a screen reader hears. The seed tags two skills
// (customer-order: sales/orders, record; supplier-risk: sales/risk, process) and leaves `customer` alone.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

type Row = Awaited<ReturnType<EscurelApi['knowledge']['getChildren']>>[number];

suite('Knowledge tree: folders, roles, previews', () => {
  let api: EscurelApi;
  suiteSetup(async () => {
    api = (await vscode.extensions.getExtension('datazoo.escurel')!.activate()) as EscurelApi;
  });

  const kids = (n?: Row) => api.knowledge.getChildren(n);
  const label = (n: Row) => (n.kind === 'folder' || n.kind === 'skill' ? n.label : n.kind);
  const find = (rows: Row[], kind: 'folder' | 'skill', l: string) =>
    rows.find((n) => n.kind === kind && n.label === l);

  test('a skill declaring folder: sits under that folder path, every segment a folder node', async () => {
    const roots = await kids();
    const sales = find(roots, 'folder', 'sales');
    assert.ok(sales, `a sales folder among ${roots.map(label).join(', ')}`);
    const inSales = await kids(sales);
    const orders = find(inSales, 'folder', 'orders');
    const risk = find(inSales, 'folder', 'risk');
    assert.ok(orders && risk, `orders and risk folders among ${inSales.map(label).join(', ')}`);
    assert.ok(
      find(await kids(orders), 'skill', 'customer-order'),
      'customer-order under sales/orders',
    );
    assert.ok(find(await kids(risk), 'skill', 'supplier-risk'), 'supplier-risk under sales/risk');
    // The folder row is a stable, expandable node.
    const item = api.knowledge.getTreeItem(sales);
    assert.equal(item.id, 'folder:sales');
    assert.equal(item.collapsibleState, vscode.TreeItemCollapsibleState.Expanded);
  });

  test('skills and instances have stable ids, so what a person expanded survives a live refresh', async () => {
    // Without an id VS Code cannot match a row across a refresh and collapses it: the tree folded
    // whatever you had opened every time anything live happened.
    const roots = await kids();
    const sales = find(roots, 'folder', 'sales')!;
    const orders = find(await kids(sales), 'folder', 'orders')!;
    const order = find(await kids(orders), 'skill', 'customer-order')!;
    assert.equal(api.knowledge.getTreeItem(order).id, 'skill:customer-order');
    const rows = await kids(order);
    const instance = rows.find((n) => n.kind === 'instance');
    if (instance) {
      assert.match(
        String(api.knowledge.getTreeItem(instance).id),
        /^instance:markdown\/instances\//,
      );
    }
  });

  test('a skill with no folder stays at the top level, after the folders', async () => {
    const roots = await kids();
    const customer = find(roots, 'skill', 'customer');
    assert.ok(customer, 'the untagged customer skill at the top level');
    const firstSkill = roots.findIndex((n) => n.kind === 'skill');
    const lastFolder = roots.map((n) => n.kind).lastIndexOf('folder');
    assert.ok(lastFolder < firstSkill, `folders first: ${roots.map(label).join(', ')}`);
  });

  test('the role icon and the accessible name come from the declared role', async () => {
    const roots = await kids();
    const sales = find(roots, 'folder', 'sales')!;
    const orders = find(await kids(sales), 'folder', 'orders')!;
    const risk = find(await kids(sales), 'folder', 'risk')!;
    const order = api.knowledge.getTreeItem(find(await kids(orders), 'skill', 'customer-order')!);
    const check = api.knowledge.getTreeItem(find(await kids(risk), 'skill', 'supplier-risk')!);
    assert.equal((order.iconPath as vscode.ThemeIcon).id, 'database');
    assert.equal((check.iconPath as vscode.ThemeIcon).id, 'play-circle');
    assert.equal(
      order.accessibilityInformation?.label,
      'record skill customer-order, autonomy review',
    );
    assert.match(String(check.accessibilityInformation?.label), /^process skill supplier-risk/);
  });

  test('instances still page beneath a skill inside a folder', async () => {
    const roots = await kids();
    const sales = find(roots, 'folder', 'sales')!;
    const orders = find(await kids(sales), 'folder', 'orders')!;
    const order = find(await kids(orders), 'skill', 'customer-order')!;
    assert.equal(api.knowledge.getTreeItem(order).contextValue, 'skill');
    // (the seed holds no customer-order instances; the call must still succeed and return rows or none)
    const rows = await kids(order);
    assert.ok(Array.isArray(rows));
  });

  // The filter narrows the SKILL level only (tags and text come from the real list_skills rows);
  // folders with nothing left disappear, and clearing it brings everything back.
  suite('filter', () => {
    teardown(() => api.knowledge.setFilter({}));

    const skillIds = async (): Promise<string[]> => {
      const out: string[] = [];
      const walk = async (rows: Row[]): Promise<void> => {
        for (const r of rows) {
          if (r.kind === 'folder') await walk(await kids(r));
          else if (r.kind === 'skill') out.push(r.label);
        }
      };
      await walk(await kids());
      return out.sort();
    };

    test('by tag: only the skills carrying it stay, with the folders that lead to them', async () => {
      const all = await skillIds();
      assert.ok(all.includes('customer-order') && all.includes('supplier-risk'), all.join(','));
      api.knowledge.setFilter({ tag: 'sap' });
      const sap = await skillIds();
      assert.deepEqual(sap, ['customer-order']);
      const roots = await kids();
      assert.ok(find(roots, 'folder', 'sales'), 'the sales folder is kept for customer-order');
      assert.ok(!find(roots, 'skill', 'customer'), 'an untagged skill is filtered out');
    });

    test('by text, combined with a tag, and cleared again', async () => {
      api.knowledge.setFilter({ text: 'risk' });
      assert.deepEqual(await skillIds(), ['supplier-risk']);
      api.knowledge.setFilter({ tag: 'sap', text: 'risk' });
      assert.deepEqual(await skillIds(), [], 'tag AND text have to match');
      api.knowledge.setFilter({});
      assert.ok((await skillIds()).length >= 3, 'clearing brings every skill back');
    });
  });
});
