import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { uriForPage } from '../fs/provider';
import { log } from '../log';
import { buildInboxRows, type InboxRow } from './inboxModel';

export interface ErrorRow {
  kind: 'error';
  message: string;
}

export type Node = InboxRow | ErrorRow;

/**
 * Inbox (SPEC §3.2): Events from `list_inbox`, newest first.
 * Selecting a row runs `escurel.openThread` (stub in M2).
 * Context menu provides "Open instance" (when event has instance_page_id)
 * and "Show Markdown".
 */
export class InboxTree implements vscode.TreeDataProvider<Node> {
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;

  constructor(private readonly client: () => EscurelClient) {}

  static register(context: vscode.ExtensionContext, client: () => EscurelClient): InboxTree {
    const tree = new InboxTree(client);
    context.subscriptions.push(
      vscode.window.createTreeView('escurel.inbox', {
        treeDataProvider: tree,
        showCollapseAll: false,
      }),
    );
    return tree;
  }

  refresh(): void {
    this.changed.fire(undefined);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'event': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.description = n.description;
        item.tooltip = n.tooltip;
        item.iconPath = new vscode.ThemeIcon(
          'symbol-event',
          new vscode.ThemeColor('charts.orange'),
        );
        item.contextValue = n.pageId ? 'event.hasInstance' : 'event';
        if (n.pageId) {
          item.resourceUri = uriForPage(n.pageId);
        }
        item.command = {
          command: 'escurel.openThread',
          title: 'Open Thread',
          arguments: [n.event.root_event_id ?? n.event.event_id],
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
        const page = await this.client().listInbox();
        await vscode.commands.executeCommand('setContext', 'escurel.connected', true);
        return buildInboxRows(page.events);
      }
      return [];
    } catch (e) {
      log().warn(`escurel: inbox tree: ${describeError(e)}`);
      if (!n) await vscode.commands.executeCommand('setContext', 'escurel.connected', false);
      return [{ kind: 'error', message: describeError(e) }];
    }
  }
}
