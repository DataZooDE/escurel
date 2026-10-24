import { errorRowSpec } from './errorRow';
import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { connectionStateOf, describeError } from '../errors';
import { uriForPage } from '../fs/provider';
import { log } from '../log';
import { decorateRows, emptyInboxMessage, type InboxDecoratorRegistry } from '../api/inbox';
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

  private view: Pick<vscode.TreeView<Node>, 'message'> & Partial<Pick<vscode.TreeView<Node>, 'badge'>> | undefined;

  constructor(
    private readonly client: () => EscurelClient,
    private readonly decorators?: InboxDecoratorRegistry,
    private readonly language: () => string = () => vscode.env.language,
  ) {}

  /** The tree view whose `message` and `badge` a companion's decorators may set. */
  attach(view: Pick<vscode.TreeView<Node>, 'message'> & Partial<Pick<vscode.TreeView<Node>, 'badge'>>): void {
    this.view = view;
  }

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
    decorators?: InboxDecoratorRegistry,
  ): InboxTree {
    const tree = new InboxTree(client, decorators);
    const view = vscode.window.createTreeView('escurel.inbox', {
      treeDataProvider: tree,
      showCollapseAll: false,
    });
    tree.attach(view);
    context.subscriptions.push(view);
    // A decorator that appears or goes while the view is open changes what it shows.
    if (decorators) context.subscriptions.push(decorators.onDidChange(() => tree.refresh()));
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
        item.iconPath =
          n.status === 'answered' || n.status === 'done'
            ? new vscode.ThemeIcon('check')
            : n.status === 'failed'
              ? new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'))
              : new vscode.ThemeIcon('symbol-event', new vscode.ThemeColor('charts.orange'));
        item.contextValue = n.pageId ? 'event.hasInstance' : 'event';
        if (n.pageId) {
          item.resourceUri = uriForPage(n.pageId);
        }
        item.command = {
          command: 'escurel.openThread',
          title: 'Open thread',
          arguments: [n.event.root_event_id ?? n.event.event_id],
        };
        return item;
      }
      case 'error': {
        const spec = errorRowSpec(n.message);
        const item = new vscode.TreeItem(spec.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('warning', new vscode.ThemeColor('errorForeground'));
        item.tooltip = spec.tooltip;
        item.command = { command: spec.command, title: 'Try again' };
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
        await vscode.commands.executeCommand('setContext', 'escurel.connectionState', 'ok');
        const plain = buildInboxRows(page.events);
        if (!this.decorators?.size) {
          if (this.view) {
            this.view.badge = undefined;
            this.view.message = undefined;
          }
          return plain;
        }
        const decorated = await decorateRows(this.decorators, plain);
        const open = decorated.open;
        if (this.view) {
          this.view.badge = open ? { value: open, tooltip: `${open} open` } : undefined;
          this.view.message = decorated.rows.length
            ? undefined
            : emptyInboxMessage(this.decorators, this.language());
        }
        return decorated.rows;
      }
      return [];
    } catch (e) {
      log().warn(`escurel: inbox tree: ${describeError(e)}`);
      if (!n) {
        await vscode.commands.executeCommand('setContext', 'escurel.connected', false);
        await vscode.commands.executeCommand(
          'setContext',
          'escurel.connectionState',
          connectionStateOf(e),
        );
      }
      return [{ kind: 'error', message: describeError(e) }];
    }
  }
}
