import * as vscode from 'vscode';
import type { NodeTarget, ThreadView } from '../shared/protocol';
import { outlineRows, type OutlineRow } from './threadsModel';

interface EmptyRow {
  kind: 'empty';
  label: string;
}

type Row = OutlineRow | EmptyRow;

function command(target: NodeTarget): vscode.Command | undefined {
  switch (target.open) {
    case 'thread':
      return {
        command: 'escurel.openThread',
        title: 'Open Thread',
        arguments: [target.rootEventId],
      };
    case 'run':
      return { command: 'escurel.openRun', title: 'Open Run', arguments: [target.runId] };
    case 'review':
      return {
        command: 'escurel.openReview',
        title: 'Open Review',
        arguments: [target.draftId ?? target.changesetId],
      };
    default:
      return undefined;
  }
}

/** The one open thread; the host owns view registration and navigation commands. */
export class ThreadsTree implements vscode.TreeDataProvider<Row> {
  private readonly changed = new vscode.EventEmitter<Row | undefined>();
  private readonly selected = new vscode.EventEmitter<string>();
  readonly onDidChangeTreeData = this.changed.event;
  readonly onDidSelect = this.selected.event;
  private view: ThreadView | undefined;
  private collapsed: ReadonlySet<string> = new Set();

  setThread(view: ThreadView | undefined): void {
    this.view = view;
    this.refresh();
  }

  refresh(): void {
    this.changed.fire(undefined);
  }

  select(nodeId: string): void {
    if (this.view?.nodes.some((node) => node.id === nodeId)) this.selected.fire(nodeId);
  }

  getTreeItem(row: Row): vscode.TreeItem {
    if ('kind' in row) return new vscode.TreeItem(row.label, vscode.TreeItemCollapsibleState.None);
    const state =
      row.collapsibleState === 'expanded'
        ? vscode.TreeItemCollapsibleState.Expanded
        : row.collapsibleState === 'collapsed'
          ? vscode.TreeItemCollapsibleState.Collapsed
          : vscode.TreeItemCollapsibleState.None;
    const item = new vscode.TreeItem(row.label, state);
    item.id = row.id;
    item.description = row.description;
    item.contextValue = row.contextValue;
    item.command = command(row.target);
    const colour =
      row.contextValue === 'escurel.event'
        ? 'charts.orange'
        : row.contextValue === 'escurel.run'
          ? row.description === 'failed' ||
            row.description === 'dead_letter' ||
            row.description === 'cancelled'
            ? 'errorForeground'
            : 'charts.green'
          : 'charts.blue';
    item.iconPath = new vscode.ThemeIcon('circle-filled', new vscode.ThemeColor(colour));
    return item;
  }

  getChildren(row?: Row): Row[] {
    if (row) return 'kind' in row ? [] : row.children;
    if (!this.view) return [{ kind: 'empty', label: 'No thread open. Open one from the Inbox.' }];
    return outlineRows(this.view, this.collapsed);
  }
}
