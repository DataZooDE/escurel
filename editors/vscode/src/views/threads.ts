import * as vscode from 'vscode';
import type { NodeTarget, ThreadView } from '../shared/protocol';
import { outlineRows, type OutlineRow } from './threadsModel';

/**
 * What the tree shows when no thread is open. A tree-view MESSAGE, not a row: an empty-state
 * row is announced by a screen reader as "1 of 1, level 1", a node that does not exist.
 *
 * It says to open an EVENT, not "a thread": there is no tool that lists threads, so nothing
 * should imply a directory of them exists.
 */
export const THREADS_EMPTY_MESSAGE = 'Select an event in the Inbox to open its thread.';

/** A distinct icon per kind, because colour alone cannot tell them apart (WCAG 1.4.1). */
const ICONS: Record<OutlineRow['kind'], string> = {
  event: 'symbol-event',
  run: 'play-circle',
  changeset: 'git-pull-request',
  draft: 'file',
};

function colourFor(row: OutlineRow): string {
  if (row.kind === 'event') return 'charts.orange';
  if (row.kind !== 'run') return 'charts.blue';
  switch (row.state) {
    case 'processed':
      return 'charts.green';
    case 'failed':
    case 'dead_letter':
    case 'cancelled':
      return 'errorForeground';
    // Running, planned, or a state this version does not know: not a success, and not
    // shown as one. The tree used to colour every run green unless its text said it failed.
    default:
      return 'charts.yellow';
  }
}

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
export class ThreadsTree implements vscode.TreeDataProvider<OutlineRow> {
  private readonly changed = new vscode.EventEmitter<OutlineRow | undefined>();
  private readonly selected = new vscode.EventEmitter<string>();
  readonly onDidChangeTreeData = this.changed.event;
  readonly onDidSelect = this.selected.event;
  private view: ThreadView | undefined;
  private collapsed: ReadonlySet<string> = new Set();

  /** Set as the tree view's `message`: shown only while there is nothing to list. */
  get message(): string | undefined {
    return this.view ? undefined : THREADS_EMPTY_MESSAGE;
  }

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

  getTreeItem(row: OutlineRow): vscode.TreeItem {
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
    item.iconPath = new vscode.ThemeIcon(ICONS[row.kind], new vscode.ThemeColor(colourFor(row)));
    // The kind is in the accessible name; the icon and colour are not relied on.
    item.accessibilityInformation = {
      label: `${row.kind} ${row.label}, ${row.description}`,
      role: 'treeitem',
    };
    return item;
  }

  getChildren(row?: OutlineRow): OutlineRow[] {
    if (row) return row.children;
    return this.view ? outlineRows(this.view, this.collapsed) : [];
  }
}
