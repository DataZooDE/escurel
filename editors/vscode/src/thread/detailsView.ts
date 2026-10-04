import * as vscode from 'vscode';
import type { DetailsHostToWebview, DetailsWebviewToHost, ShownDetails } from '../shared/protocol';
import { safePost } from '../shared/safePost';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { log } from '../log';
import { acceptDetailsAction, type Shown } from './detailsRouting';
import type { ThreadController } from './controller';
import { isThreadViewType, shouldShowDetails } from './detailsFollowsEditor';

/**
 * The details of the node selected in a thread, as a WebviewView in the PANEL area (SPEC §3.5).
 *
 * VS Code owns where it sits and how big it is: the user drags it to the side, drops it into the
 * sidebar or resizes it, and there is no dock code here. The host decides what it shows (the most
 * recently selected node of any open thread) and what it may do (only for that thread, validated
 * again by the thread's own message handler); the webview only renders and posts back.
 */
export class DetailsViewProvider implements vscode.WebviewViewProvider, vscode.Disposable {
  static readonly viewType = 'escurel.details';

  private view: vscode.WebviewView | undefined;
  private shown: ShownDetails | undefined;
  /** The panel is brought up on the first selection of a session, never again unless the user asks. */
  private broughtUp = false;
  private readonly subs: vscode.Disposable[] = [];
  private viewSub: vscode.Disposable | undefined;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly threads: ThreadController,
  ) {
    // It follows the editor in front: a node of a thread is meaningless beside an order page.
    this.subs.push(
      vscode.window.tabGroups.onDidChangeTabs(() => this.followEditor()),
      vscode.window.tabGroups.onDidChangeTabGroups(() => this.followEditor()),
    );
    this.subs.push(
      threads.onDidChangeDetails((e) => {
        if (e.nodeId !== undefined && e.detail) {
          this.update({ rootEventId: e.rootEventId, nodeId: e.nodeId, detail: e.detail }, e.reason);
        } else if (this.shown?.rootEventId === e.rootEventId) {
          this.clear();
        }
      }),
    );
  }

  static register(
    context: vscode.ExtensionContext,
    threads: ThreadController,
  ): DetailsViewProvider {
    const provider = new DetailsViewProvider(context, threads);
    context.subscriptions.push(
      provider,
      vscode.window.registerWebviewViewProvider(DetailsViewProvider.viewType, provider, {
        // Selecting a node while the panel is hidden must not lose it; it is shown when opened.
        webviewOptions: { retainContextWhenHidden: true },
      }),
    );
    return provider;
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = webviewOptions(this.context.extensionUri);
    view.webview.html = webviewHtml(
      view.webview,
      this.context.extensionUri,
      'details',
      'escurel-details',
    );
    const sub = view.webview.onDidReceiveMessage((m: unknown) => void this.handleMessage(m));
    this.viewSub?.dispose();
    this.viewSub = sub;
    view.onDidDispose(() => {
      sub.dispose();
      if (this.view === view) this.view = undefined;
    });
  }

  /** What the view shows now (for the integration suite, which cannot read a webview). */
  current(): ShownDetails | undefined {
    return this.shown;
  }

  /**
   * A message from the view. Public so the integration suite can exercise the host's handling
   * without a click in a webview it cannot reach. Returns whether the host acted on it.
   */
  async handleMessage(raw: unknown): Promise<boolean> {
    const m = raw as DetailsWebviewToHost | undefined;
    if (!m || typeof m !== 'object') return false;
    if (m.type === 'ready') {
      this.post(
        this.shown && this.visibleNode
          ? { type: 'details', ...this.shown }
          : { type: 'details-empty' },
      );
      return true;
    }
    if (m.type === 'focus-canvas') {
      if (typeof m.rootEventId !== 'string' || m.rootEventId !== this.shown?.rootEventId) {
        return false;
      }
      this.threads.focusCanvas(m.rootEventId);
      return true;
    }
    const accepted = acceptDetailsAction(this.scope(), (id) => this.threads.isOpen(id), raw);
    if (!accepted) {
      log().warn('details: refused a message for a thread that is not the one shown');
      return false;
    }
    return this.threads.handleWebviewMessage(accepted.rootEventId, accepted.message);
  }

  /** What the node on show offers: the only things the view may act on. */
  private scope(): Shown | undefined {
    const shown = this.shown;
    if (!shown) return undefined;
    const actions = shown.detail.actions;
    return {
      rootEventId: shown.rootEventId,
      nodeId: shown.nodeId,
      pageId: actions?.skills?.pageId,
      skills: [
        ...(actions?.skills?.actions.map((a) => a.skill) ?? []),
        ...(actions?.skill ? [actions.skill] : []),
      ],
    };
  }

  private update(next: ShownDetails, reason: 'select' | 'refresh'): void {
    const same = this.shown?.rootEventId === next.rootEventId && this.shown.nodeId === next.nodeId;
    // A live reload of a thread the user is not looking at must not steal the view: the details
    // follow the most recently SELECTED node.
    if (reason === 'refresh' && !same) return;
    this.shown = next;
    this.visibleNode = true;
    this.post({ type: 'details', ...next });
    if (reason === 'select') void this.bringUp(next.rootEventId);
  }

  /** Whether the view is showing its node (false while another kind of editor is in front). */
  private visibleNode = true;

  private followEditor(): void {
    const tab = vscode.window.tabGroups.activeTabGroup?.activeTab;
    const input = tab?.input;
    const show = shouldShowDetails({
      hasSelection: this.shown !== undefined,
      activeIsThread: input instanceof vscode.TabInputWebview && isThreadViewType(input.viewType),
      activeIsNone: tab === undefined,
    });
    if (show === this.visibleNode) return;
    this.visibleNode = show;
    if (show && this.shown) this.post({ type: 'details', ...this.shown });
    else this.post({ type: 'details-empty' });
  }

  /** Whether the view is currently showing its selected node (for the integration suite). */
  showing(): boolean {
    return this.visibleNode && this.shown !== undefined;
  }

  private clear(): void {
    this.shown = undefined;
    this.post({ type: 'details-empty' });
  }

  private post(m: DetailsHostToWebview): void {
    if (this.view) safePost(this.view, m);
  }

  /**
   * Shows the panel without taking the focus from the canvas, the first time in a session. A view
   * that was never opened has no webview to `show`, so it is focused through its own command and
   * the thread's canvas is given the focus straight back.
   */
  private async bringUp(rootEventId: string): Promise<void> {
    if (this.broughtUp) return;
    try {
      if (this.view) {
        this.view.show(true);
      } else {
        // Only give the focus back if the canvas had it: a selection made from the outline
        // must not be pulled over to the editor.
        const canvasHadFocus = this.threads.isActive(rootEventId);
        await vscode.commands.executeCommand('escurel.details.focus');
        if (canvasHadFocus) this.threads.focusCanvas(rootEventId);
      }
      // Only once it has worked: a failed attempt must not stop the next selection from trying.
      this.broughtUp = true;
    } catch (err) {
      log().warn(`details: could not bring the panel up: ${String(err)}`);
    }
  }

  dispose(): void {
    for (const s of this.subs) s.dispose();
    this.viewSub?.dispose();
    this.view = undefined;
  }
}
