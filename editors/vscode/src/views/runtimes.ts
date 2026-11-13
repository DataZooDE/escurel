import * as vscode from 'vscode';
import { describeError } from '../errors';
import type { Services } from '../services';
import { LazyFlatTree, type FlatRead } from './lazyTree';
import { runtimeRows, type RuntimeRow, type RuntimeStatus } from './runtimesModel';
import { VisibleTicker } from './visibleTicker';

/** How often a visible Runtimes view re-reads, so liveness on screen keeps up by itself. */
const REFRESH_MS = 15_000;

/** Shape and colour both differ, and the description says the status in words as well. */
const STATUS_ICON: Record<RuntimeStatus, [string, string | undefined]> = {
  online: ['pass-filled', 'charts.green'],
  stale: ['warning', 'editorWarning.foreground'],
  stopped: ['debug-stop', undefined],
  unknown: ['question', undefined],
};

/**
 * The Runtimes view (Runs side bar): every registered runner (`list_runtimes`, #818) with whether it
 * is alive, the harnesses and CLI versions it offers, its labels and how many runs it has live. A
 * click opens the runtime page.
 */
export class RuntimesTree extends LazyFlatTree<RuntimeRow> {
  constructor(services: Services) {
    super(services, 'Open this view to list runtimes.');
  }

  protected async read(): Promise<FlatRead<RuntimeRow>> {
    let runtimes;
    try {
      ({ runtimes } = await this.services.client.listRuntimes());
    } catch (error) {
      return { rows: [], message: `Could not list runtimes: ${describeError(error)}` };
    }
    if (!runtimes.length)
      return {
        rows: [],
        message: 'No runtimes yet. A runner registers itself here when it starts.',
      };
    const rows = runtimeRows(runtimes);
    const online = rows.filter((r) => r.status === 'online').length;
    return { rows, message: `${online} of ${rows.length} online` };
  }

  getTreeItem(row: RuntimeRow): vscode.TreeItem {
    const item = new vscode.TreeItem(row.label, vscode.TreeItemCollapsibleState.None);
    item.id = row.id;
    item.description = row.description;
    item.tooltip = row.tooltip;
    const [icon, colour] = STATUS_ICON[row.status];
    item.iconPath = new vscode.ThemeIcon(icon, colour ? new vscode.ThemeColor(colour) : undefined);
    item.accessibilityInformation = { label: `Runtime ${row.label}: ${row.description}` };
    item.contextValue = `runtime.${row.status}`;
    item.command = { command: 'escurel.openPage', title: 'Open runtime', arguments: [row.pageId] };
    return item;
  }
}

export function registerRuntimesView(
  context: vscode.ExtensionContext,
  services: Services,
): RuntimesTree {
  const tree = new RuntimesTree(services);
  const view = vscode.window.createTreeView('escurel.runtimes', {
    treeDataProvider: tree,
    showCollapseAll: false,
  });
  tree.bindView(view);
  const ticker = new VisibleTicker(REFRESH_MS, () => void tree.load());
  ticker.setVisible(view.visible);
  context.subscriptions.push(
    ticker,
    view.onDidChangeVisibility((e) => ticker.setVisible(e.visible)),
    tree,
    view,
    vscode.commands.registerCommand('escurel.runtimes.refresh', () => tree.load()),
  );
  return tree;
}
