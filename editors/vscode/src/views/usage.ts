import * as vscode from 'vscode';
import type { UsageDimension } from '../client/types';
import type { Services } from '../services';
import { LazyFlatTree, type FlatRead } from './lazyTree';
import {
  USAGE_GROUPS,
  USAGE_WINDOWS,
  usageFailure,
  usageRange,
  usageRows,
  usageTitle,
  type UsageRow,
  type UsageWindow,
} from './usageModel';

/**
 * The Usage view (Runs side bar): what runs cost over a window, grouped by one dimension, from the
 * admin tool `usage_report`. A non-admin sees one line saying so, not an error. It reads only while
 * it is visible: a cost report is not something every window should ask for on start.
 */
export class UsageTree extends LazyFlatTree<UsageRow> {
  private window: UsageWindow = '7d';
  private groupBy: UsageDimension = 'skill';

  constructor(services: Services) {
    super(services, 'Open this view to read usage.');
  }

  get settings(): { window: UsageWindow; groupBy: UsageDimension } {
    return { window: this.window, groupBy: this.groupBy };
  }

  async setWindow(window: UsageWindow): Promise<void> {
    this.window = window;
    await this.load();
  }

  async setGroupBy(groupBy: UsageDimension): Promise<void> {
    this.groupBy = groupBy;
    await this.load();
  }

  protected async read(): Promise<FlatRead<UsageRow>> {
    const { window, groupBy } = this;
    try {
      const report = await this.services.client.usageReport({
        group_by: [groupBy],
        ...usageRange(window, new Date()),
      });
      const rows = usageRows(report, groupBy);
      const label = USAGE_WINDOWS.find((w) => w.id === window)!.label.toLowerCase();
      return {
        rows,
        message: rows.length ? usageTitle(window, groupBy) : `No run finished in the ${label}.`,
      };
    } catch (error) {
      return { rows: [], message: usageFailure(error).message };
    }
  }

  getTreeItem(row: UsageRow): vscode.TreeItem {
    const item = new vscode.TreeItem(row.label, vscode.TreeItemCollapsibleState.None);
    item.id = row.id;
    item.description = row.description;
    item.tooltip = row.tooltip;
    item.iconPath = new vscode.ThemeIcon(row.kind === 'total' ? 'graph' : 'circle-small');
    item.accessibilityInformation = { label: `${row.label}: ${row.description}` };
    item.contextValue = `usage-${row.kind}`;
    return item;
  }
}

export function registerUsageView(context: vscode.ExtensionContext, services: Services): UsageTree {
  const tree = new UsageTree(services);
  const view = vscode.window.createTreeView('escurel.usage', {
    treeDataProvider: tree,
    showCollapseAll: false,
  });
  tree.bindView(view);
  context.subscriptions.push(
    tree,
    view,
    vscode.commands.registerCommand('escurel.usage.window', async () => {
      const picked = await vscode.window.showQuickPick(
        USAGE_WINDOWS.map((w) => ({
          label: w.label,
          id: w.id,
          picked: w.id === tree.settings.window,
        })),
        { placeHolder: 'Show usage for…' },
      );
      if (picked) await tree.setWindow(picked.id);
    }),
    vscode.commands.registerCommand('escurel.usage.groupBy', async () => {
      const picked = await vscode.window.showQuickPick(
        USAGE_GROUPS.map((g) => ({
          label: `By ${g.label}`,
          id: g.id,
          picked: g.id === tree.settings.groupBy,
        })),
        { placeHolder: 'Group usage by…' },
      );
      if (picked) await tree.setGroupBy(picked.id);
    }),
    vscode.commands.registerCommand('escurel.usage.refresh', () => tree.load()),
  );
  return tree;
}
