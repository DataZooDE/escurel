import * as vscode from 'vscode';
import type { NodeTarget, ThreadView } from '../shared/protocol';
import { commandForTarget } from '../thread/nodeTarget';
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

/** The command a row runs. Shared with the canvas, so a click and a row land in one place. */
function command(target: NodeTarget): vscode.Command | undefined {
  const routed = commandForTarget(target);
  return routed ? { command: routed.command, title: 'Open', arguments: routed.args } : undefined;
}

/** The one open thread; the host owns view registration and navigation commands. */
export class ThreadsTree implements vscode.TreeDataProvider<OutlineRow> {
  private readonly changed = new vscode.EventEmitter<OutlineRow | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private view: ThreadView | undefined;
  private collapsed: ReadonlySet<string> = new Set();
  /** The rows last handed to the tree. `reveal` needs the SAME objects, not rebuilt ones. */
  private rows: OutlineRow[] = [];
  private readonly parents = new Map<string, OutlineRow | undefined>();

  /** Set as the tree view's `message`: shown only while there is nothing to list. */
  get message(): string | undefined {
    return this.view ? undefined : THREADS_EMPTY_MESSAGE;
  }

  setThread(view: ThreadView | undefined): void {
    this.view = view;
    this.rows = view ? outlineRows(view, this.collapsed) : [];
    this.parents.clear();
    const index = (rows: OutlineRow[], parent?: OutlineRow) => {
      for (const r of rows) {
        this.parents.set(r.id, parent);
        index(r.children, r);
      }
    };
    index(this.rows);
    this.refresh();
  }

  /** The row for a node id, for `TreeView.reveal`. */
  rowFor(nodeId: string): OutlineRow | undefined {
    const find = (rows: OutlineRow[]): OutlineRow | undefined => {
      for (const r of rows) {
        if (r.id === nodeId) return r;
        const hit = find(r.children);
        if (hit) return hit;
      }
      return undefined;
    };
    return find(this.rows);
  }

  getParent(row: OutlineRow): OutlineRow | undefined {
    return this.parents.get(row.id);
  }

  refresh(): void {
    this.changed.fire(undefined);
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
    return row ? row.children : this.rows;
  }
}
