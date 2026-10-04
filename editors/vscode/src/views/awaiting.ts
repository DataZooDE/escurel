import { errorRowSpec } from './errorRow';
import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { connectionStateOf, describeError } from '../errors';
import { log } from '../log';
import { loadPlanInputs } from './planInputs';
import { buildAwaitingRows, type AwaitingRow } from './awaitingModel';
import { awaitingDisplay } from './awaitingDisplay';

export interface ErrorRow {
  kind: 'error';
  message: string;
}

export type Node = AwaitingRow | ErrorRow;

/**
 * Awaiting you (SPEC §3.2): The queue of items awaiting human review or confirmation.
 * Merged from open changesets, open unparented drafts, and confirm gates.
 * Badge displays the total count of awaiting items.
 * Selecting a row runs `escurel.openReview`.
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

  get badge(): vscode.ViewBadge | undefined {
    return this.treeView?.badge;
  }

  refresh(): void {
    this.changed.fire(undefined);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    const shown = n.kind === 'error' ? undefined : awaitingDisplay(n, Date.now());
    switch (n.kind) {
      case 'changeset': {
        const item = new vscode.TreeItem(shown!.label, vscode.TreeItemCollapsibleState.None);
        item.description = shown!.description;
        item.tooltip = `${shown!.kind}\n${n.label} · ${shown!.description}`;
        item.accessibilityInformation = { label: `${shown!.label}. ${shown!.description}` };
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
        const item = new vscode.TreeItem(shown!.label, vscode.TreeItemCollapsibleState.None);
        item.description = shown!.description;
        item.tooltip = `${shown!.kind}\n${n.label} · ${shown!.description}`;
        item.accessibilityInformation = { label: `${shown!.label}. ${shown!.description}` };
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
        const item = new vscode.TreeItem(shown!.label, vscode.TreeItemCollapsibleState.None);
        item.description = shown!.description;
        item.tooltip = `${shown!.kind}\n${n.label} · ${shown!.description}`;
        item.accessibilityInformation = { label: `${shown!.label}. ${shown!.description}` };
        item.iconPath = new vscode.ThemeIcon('bell', new vscode.ThemeColor('charts.orange'));
        item.contextValue = 'awaiting.confirm_gate';
        item.command = {
          command: 'escurel.openReview',
          title: 'Open Review',
          arguments: [n],
        };
        return item;
      }
      case 'plan': {
        const item = new vscode.TreeItem(shown!.label, vscode.TreeItemCollapsibleState.None);
        item.description = shown!.description;
        item.tooltip = `${shown!.kind}.`;
        item.accessibilityInformation = { label: `${shown!.label}. ${shown!.description}` };
        item.iconPath = new vscode.ThemeIcon('checklist', new vscode.ThemeColor('charts.orange'));
        item.contextValue = 'awaiting.plan';
        item.command = {
          // Opening is safe; approving is the inline button, and asks for confirmation.
          command: 'escurel.openRun',
          title: 'Open run',
          arguments: [{ runId: n.runId }],
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
        const [changesets, drafts, inboxPage, skills, plans] = await Promise.all([
          this.client().listChangesets(),
          this.client().listDrafts(),
          this.client().listInbox(),
          this.client().listSkills(),
          // Plans waiting for approval; a failure here must not empty the whole queue.
          loadPlanInputs(this.client()),
        ]);
        await vscode.commands.executeCommand('setContext', 'escurel.connected', true);
        await vscode.commands.executeCommand('setContext', 'escurel.connectionState', 'ok');
        const rows = buildAwaitingRows({
          changesets,
          drafts,
          events: inboxPage.events,
          skills,
          runEvents: plans.runEvents,
          userEvents: plans.userEvents,
        });
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
