import * as vscode from 'vscode';
import type { Services } from '../services';

/** What one read of a flat view produced: its rows and the one-line message above them. */
export interface FlatRead<Row> {
  rows: Row[];
  message: string | undefined;
}

/**
 * A flat tree that reads only while it is visible, and again when the gateway or sign-in changes
 * (then only once it is shown). Overlapping reads are settled by order: the newest to start wins.
 * Subclasses say how to read and how a row looks.
 */
export abstract class LazyFlatTree<Row> implements vscode.TreeDataProvider<Row>, vscode.Disposable {
  private readonly changed = new vscode.EventEmitter<Row | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private current: Row[] = [];
  private note: string | undefined;
  private view?: vscode.TreeView<Row>;
  private stale = true;
  private seq = 0;
  private readonly subs: vscode.Disposable[] = [];

  protected constructor(
    protected readonly services: Services,
    initialMessage: string,
  ) {
    this.note = initialMessage;
    this.subs.push(
      this.changed,
      services.onDidChange(() => {
        this.stale = true;
        if (this.view?.visible) void this.load();
      }),
    );
  }

  protected abstract read(): Promise<FlatRead<Row>>;
  abstract getTreeItem(row: Row): vscode.TreeItem;

  bindView(view: vscode.TreeView<Row>): void {
    this.view = view;
    this.subs.push(
      view.onDidChangeVisibility((e) => {
        if (e.visible && this.stale) void this.load();
      }),
    );
    if (view.visible) void this.load();
    this.publish();
  }

  /** The rows on show (the integration suites read them). */
  rows(): readonly Row[] {
    return this.current;
  }

  /** The view's message: what it shows, or why it shows nothing. */
  get message(): string | undefined {
    return this.note;
  }

  async load(): Promise<void> {
    const mine = ++this.seq;
    this.stale = false;
    const result = await this.read();
    if (mine !== this.seq) return;
    this.current = result.rows;
    this.note = result.message;
    this.publish();
  }

  private publish(): void {
    if (this.view) this.view.message = this.note;
    this.changed.fire(undefined);
  }

  getChildren(row?: Row): Row[] {
    return row ? [] : this.current;
  }

  dispose(): void {
    for (const s of this.subs) s.dispose();
  }
}
