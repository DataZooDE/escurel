import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { log } from '../log';
import { buildAwaitingRows, type AwaitingRow } from './awaitingModel';

export interface ErrorRow {
  kind: 'error';
  message: string;
}

export type Node = AwaitingRow | ErrorRow;

/**
 * Awaiting you (SPEC §3.2): The queue of items awaiting human review or confirmation.
 * Merged from open changesets, open unparented drafts, and confirm gates.
 * Badge displays the total count of awaiting items.
 * Selecting a row runs `escurel.openReview` (stub in M2).
 */
export class AwaitingTree implements vscode.TreeDataProvider<Node> {
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private treeView?: vscode.TreeView<Node>;

  constructor(private readonly client: () => EscurelClient) {}

  static register(context: vscode.ExtensionContext, client: () => EscurelClient): AwaitingTree {
    const tree = new AwaitingTree(client);
    const treeView = vscode.window.createTreeView('escurel.awaiting', {
      treeDataProvider: tree,
      showCollapseAll: false,
    });
    tree.treeView = treeView;
    context.subscriptions.push(treeView);
    return tree;
  }

  refresh(): void {
    this.changed.fire(undefined);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'changeset': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.description = n.description;
        item.tooltip = `Changeset ${n.changeset.changeset_id}: ${n.description}`;
        item.iconPath = new vscode.ThemeIcon(
          'git-pull-request',
          new vscode.ThemeColor('charts.orange'),
        );
        item.contextValue = 'awaiting.changeset';
        item.command = {
          command: 'escurel.openReview',
          title: 'Open Review',
          arguments: [n],
        };
        return item;
      }
      case 'draft': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.description = n.description;
        item.tooltip = `Draft on ${n.label} by ${n.description}`;
        item.iconPath = new vscode.ThemeIcon('edit', new vscode.ThemeColor('charts.orange'));
        item.contextValue = 'awaiting.draft';
        item.command = {
          command: 'escurel.openReview',
          title: 'Open Review',
          arguments: [n],
        };
        return item;
      }
      case 'confirm_gate': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.description = n.description;
        item.tooltip = `Confirm gate: ${n.label} (${n.description})`;
        item.iconPath = new vscode.ThemeIcon('bell', new vscode.ThemeColor('charts.orange'));
        item.contextValue = 'awaiting.confirm_gate';
        item.command = {
          command: 'escurel.openReview',
          title: 'Open Review',
          arguments: [n],
        };
        return item;
      }
      case 'error': {
        const item = new vscode.TreeItem(n.message, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'));
        item.contextValue = 'error';
        return item;
      }
    }
  }

  async getChildren(n?: Node): Promise<Node[]> {
    try {
      if (!n) {
        const [changesets, drafts, inboxPage, skills] = await Promise.all([
          this.client().listChangesets(),
          this.client().listDrafts(),
          this.client().listInbox(),
          this.client().listSkills(),
        ]);
        await vscode.commands.executeCommand('setContext', 'escurel.connected', true);
        const rows = buildAwaitingRows({ changesets, drafts, events: inboxPage.events, skills });
        if (this.treeView) {
          this.treeView.badge =
            rows.length > 0
              ? { value: rows.length, tooltip: `${rows.length} awaiting` }
              : undefined;
        }
        return rows;
      }
      return [];
    } catch (e) {
      log().warn(`escurel: awaiting tree: ${describeError(e)}`);
      if (this.treeView) {
        this.treeView.badge = undefined;
      }
      if (!n) await vscode.commands.executeCommand('setContext', 'escurel.connected', false);
      return [{ kind: 'error', message: describeError(e) }];
    }
  }
}
