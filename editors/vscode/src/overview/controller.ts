import * as vscode from 'vscode';
import { describeError } from '../errors';
import type { FocusMode } from '../focus/focusMode';
import { log } from '../log';
import type {
  OverviewHostToWebview,
  OverviewTile,
  OverviewWebviewToHost,
} from '../shared/protocol';
import { safePost } from '../shared/safePost';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import type { Services } from '../services';
import { loadOverviewInputs } from './load';
import { buildOverview, type OverviewAction } from './model';

const REFRESH_MS = 20_000;

/** The full view each tile belongs to: a fixed table, so a message can name a tile and nothing else. */
const TILE_VIEW: Readonly<Record<OverviewTile['id'], string>> = {
  decisions: 'escurel.focusAwaiting',
  agents: 'escurel.focusRuns',
  attention: 'escurel.focusRuns',
  recent: 'escurel.focusRuns',
  open: 'escurel.focusKnowledge',
};

/**
 * The overview board (SPEC §3.10): one panel, the first screen of the calm window. It reads what the
 * other views read (the queue, the runs, the skills) and answers "what needs me today". It never names a
 * command to the webview: lines carry keys, and the host resolves a key against the view it last sent.
 */
export class OverviewController implements vscode.Disposable {
  private panel: vscode.WebviewPanel | undefined;
  private actions = new Map<string, OverviewAction>();
  private readonly skillCache = new Map<string, string>();
  private timer: NodeJS.Timeout | undefined;
  private loadSeq = 0;
  private readonly disposers: vscode.Disposable[] = [];

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly services: Services,
    private readonly focus: FocusMode,
  ) {
    this.disposers.push(
      services.onDidChange(() => {
        this.skillCache.clear();
        this.actions = new Map();
        void this.load();
      }),
    );
  }

  static register(
    context: vscode.ExtensionContext,
    services: Services,
    focus: FocusMode,
  ): OverviewController {
    const c = new OverviewController(context, services, focus);
    context.subscriptions.push(
      c,
      vscode.commands.registerCommand('escurel.openOverview', () => c.open()),
    );
    return c;
  }

  /** What the board last offered, for tests: the keys the host will resolve. */
  get offered(): ReadonlyMap<string, OverviewAction> {
    return this.actions;
  }

  open(): void {
    if (this.panel) {
      this.panel.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      'escurel.overview',
      'Overview',
      vscode.ViewColumn.Active,
      { ...webviewOptions(this.context.extensionUri), retainContextWhenHidden: true },
    );
    panel.webview.html = webviewHtml(
      panel.webview,
      this.context.extensionUri,
      'overview',
      'escurel-overview',
    );
    this.panel = panel;
    const sub = panel.webview.onDidReceiveMessage((m: unknown) => void this.onMessage(m));
    const viewSub = panel.onDidChangeViewState((e) => {
      if (e.webviewPanel.visible) void this.load();
    });
    this.timer = setInterval(() => {
      if (this.panel?.visible) void this.load();
    }, REFRESH_MS);
    panel.onDidDispose(() => {
      clearInterval(this.timer);
      this.timer = undefined;
      sub.dispose();
      viewSub.dispose();
      this.panel = undefined;
      this.actions = new Map();
      this.loadSeq += 1;
    });
  }

  /** The panel and the integration tests use this same guarded path. */
  async onMessage(raw: unknown): Promise<boolean> {
    const m = raw as OverviewWebviewToHost | undefined;
    if (!m || typeof m !== 'object') return false;
    switch (m.type) {
      case 'ready':
      case 'refresh':
        await this.load();
        return true;
      case 'open': {
        const action = typeof m.key === 'string' ? this.actions.get(m.key) : undefined;
        if (!action) {
          log().warn('overview: refused open for a key the board did not offer');
          return false;
        }
        await vscode.commands.executeCommand(action.command, ...action.args);
        return true;
      }
      case 'open-tile': {
        const command = Object.hasOwn(TILE_VIEW, m.tile) ? TILE_VIEW[m.tile] : undefined;
        if (!command) return false;
        await vscode.commands.executeCommand(command);
        return true;
      }
      case 'toggle-focus':
        if (this.focus.isOn()) await this.focus.exit();
        else await this.focus.enter({ silent: true, overview: false });
        await this.load();
        return true;
      default:
        return false;
    }
  }

  private post(m: OverviewHostToWebview): void {
    if (this.panel) safePost(this.panel, m);
  }

  async load(): Promise<void> {
    if (!this.panel) return;
    const mine = ++this.loadSeq;
    try {
      const inputs = await loadOverviewInputs(this.services.client, this.skillCache);
      // Only the newest read lands, and never into a panel that was closed meanwhile.
      if (!this.panel || mine !== this.loadSeq) return;
      const out = buildOverview({ ...inputs, nowMs: Date.now(), focusOn: this.focus.isOn() });
      this.actions = out.actions;
      this.post({ type: 'overview', view: out.view });
    } catch (err) {
      if (!this.panel || mine !== this.loadSeq) return;
      this.actions = new Map();
      this.post({ type: 'overview-error', message: describeError(err) });
    }
  }

  dispose(): void {
    this.panel?.dispose();
    for (const d of this.disposers) d.dispose();
    this.disposers.length = 0;
  }
}
