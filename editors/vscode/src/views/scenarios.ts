import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { log } from '../log';
import type { Services } from '../services';
import {
  ComparisonNotFoundError,
  comparisonSummary,
  comparisonTexts,
  fetchComparison,
  matchesPage,
  type Comparison,
} from '../evolve/scenarioDiff';
import {
  SCENARIO_SCHEME,
  comparisonPageRows,
  comparisonRequestPage,
  comparisonUri,
  parseComparisonUri,
  shouldPoll,
  tableRows,
  type ComparisonPageRow,
  type EmptyRow,
  type TableRow,
  type UnverifiedRow,
} from './scenariosModel';

interface MessageRow {
  kind: 'message';
  label: string;
  command?: vscode.Command;
}

type Node =
  | ComparisonPageRow
  | (TableRow & { comparison: string; pageResultSha256: string | undefined })
  | EmptyRow
  | UnverifiedRow
  | MessageRow;

interface Loaded {
  comparison: Comparison;
  verified: boolean;
}

const BASELINES = [
  { label: 'Seed', description: 'the initial program', value: 'seed' },
  {
    label: 'Parent of the winner',
    description: 'the program the winner was derived from',
    value: 'parent',
  },
];

/**
 * Scenarios: what an Evolve candidate changed against its baseline, per state table.
 *
 * Each comparison is an owner-private Escurel page. The owner requests it, Evolve computes it
 * and fills the page in, and the row-level changes stay in Evolve's immutable record. This view
 * lists the pages, reads the record from Evolve over the signed-in token, and shows it only when
 * the page carries the record's hash; anything else is shown as unverified. It only reads: it
 * never applies a scenario to anything.
 */
export class ScenariosTree
  implements vscode.TreeDataProvider<Node>, vscode.TextDocumentContentProvider
{
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly loaded = new Map<string, Promise<Loaded>>();
  private view: vscode.TreeView<Node> | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly client: () => EscurelClient,
    private readonly services: Services,
  ) {}

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
    services: Services,
  ): ScenariosTree {
    const tree = new ScenariosTree(client, services);
    const view = vscode.window.createTreeView('escurel.scenarios', { treeDataProvider: tree });
    tree.view = view;
    context.subscriptions.push(
      view,
      // Signing in, a gateway change, or `Escurel: Refresh` reloads this view like the others.
      services.onDidChange(() => tree.refresh()),
      // Becoming visible is the moment a stale list matters.
      view.onDidChangeVisibility((e) => {
        if (e.visible) tree.refresh();
      }),
      { dispose: () => tree.stopPolling() },
      vscode.workspace.registerTextDocumentContentProvider(SCENARIO_SCHEME, tree),
      vscode.commands.registerCommand('escurel.scenarios.refresh', () => tree.refresh()),
      vscode.commands.registerCommand(
        'escurel.scenarios.compare',
        (comparison: string, table: string, pageResultSha256?: string) =>
          tree.compare(comparison, table, pageResultSha256),
      ),
      vscode.commands.registerCommand('escurel.scenarios.new', () => tree.create()),
    );
    return tree;
  }

  refresh(): void {
    this.loaded.clear();
    this.changed.fire(undefined);
  }

  stopPolling(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
  }

  /** Look again shortly while a comparison is waiting for Evolve; stop once nothing is. */
  private schedulePoll(rows: ComparisonPageRow[]): void {
    this.stopPolling();
    if (!shouldPoll(rows, this.view?.visible ?? false)) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.refresh();
    }, 5_000);
  }

  private load(comparison: string, pageResultSha256: string | undefined): Promise<Loaded> {
    const key = `${comparison}:${pageResultSha256 ?? ''}`;
    let pending = this.loaded.get(key);
    if (!pending) {
      pending = fetchComparison(
        readConfig().evolveEndpoint,
        this.services.auth.refresher,
        comparison,
      ).then((result) => ({ comparison: result, verified: matchesPage(result, pageResultSha256) }));
      // A failure must not be cached: the next click retries.
      pending.catch(() => this.loaded.delete(key));
      this.loaded.set(key, pending);
    }
    return pending;
  }

  async compare(comparison: string, table: string, pageResultSha256?: string): Promise<void> {
    try {
      const { comparison: result, verified } = await this.load(comparison, pageResultSha256);
      if (!verified) {
        void vscode.window.showWarningMessage(
          'This comparison page does not match Evolve’s record, so nothing from it is shown.',
        );
        return;
      }
      // Before the diff opens: VS Code asks for the documents' content immediately, and the
      // provider serves only comparisons it was told were verified against this page hash.
      this.pageHash.set(comparison, pageResultSha256);
      await vscode.commands.executeCommand(
        'vscode.diff',
        vscode.Uri.parse(comparisonUri(comparison, table, 'baseline')),
        vscode.Uri.parse(comparisonUri(comparison, table, 'candidate')),
        `${comparison} · ${table}: baseline ↔ candidate`,
      );
      const lines = comparisonSummary(result);
      void vscode.window.setStatusBarMessage(lines[lines.length - 1] ?? '', 8000);
    } catch (e) {
      void vscode.window.showErrorMessage(describeError(e));
    }
  }

  /** The page hash a comparison was opened with, so the diff documents serve only verified content. */
  private readonly pageHash = new Map<string, string | undefined>();

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const parsed = parseComparisonUri(uri.toString());
    if (!parsed || !this.pageHash.has(parsed.comparison)) return '';
    const { comparison, verified } = await this.load(
      parsed.comparison,
      this.pageHash.get(parsed.comparison),
    );
    if (!verified) return '';
    const texts = comparisonTexts(comparison, parsed.table);
    return parsed.side === 'baseline' ? texts.baseline : texts.candidate;
  }

  async create(): Promise<void> {
    try {
      const owner = await this.services.subject();
      if (!owner) throw new Error('Sign in before requesting a comparison.');
      const experiments = await this.client().listInstancesPage({
        skill_id: 'evolve_experiment',
        limit: 100,
      });
      const choices = experiments.instances.map((instance) => {
        const id =
          typeof instance.frontmatter.id === 'string' && instance.frontmatter.id
            ? instance.frontmatter.id
            : (instance.page_id.split('/').pop() ?? '').replace(/\.md$/, '');
        const status =
          typeof instance.frontmatter.status === 'string' ? instance.frontmatter.status : '';
        return { label: id, description: status };
      });
      if (!choices.length) {
        void vscode.window.showInformationMessage('No Evolve experiments to compare yet.');
        return;
      }
      const experiment = await vscode.window.showQuickPick(choices, {
        placeHolder: 'Which experiment’s winner should be compared?',
      });
      if (!experiment) return;
      const baseline = await vscode.window.showQuickPick(BASELINES, {
        placeHolder: 'Compare the winner against…',
      });
      if (!baseline) return;
      const id = `cmp-${experiment.label}-${baseline.value}-${Date.now().toString(36)}`.slice(
        0,
        128,
      );
      const content = comparisonRequestPage({
        id,
        owner,
        experiment: experiment.label,
        baseline: baseline.value,
      });
      const pageId = `markdown/instances/evolve_comparison/${id}.md`;
      const written = await this.client().updatePage({
        page_id: pageId,
        content,
        base_sha256: '',
      });
      if (!written.ok) {
        throw new Error(
          `Escurel did not accept the comparison page: ${JSON.stringify(written.issues ?? [])}`,
        );
      }
      this.refresh();
      await vscode.commands.executeCommand('escurel.openPage', pageId);
      void vscode.window.showInformationMessage(
        'Comparison requested. Click Compute comparison on the page to run it.',
      );
    } catch (e) {
      void vscode.window.showErrorMessage(describeError(e));
    }
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'comparison': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.Collapsed);
        item.description = n.description;
        item.iconPath = new vscode.ThemeIcon(
          n.status === 'completed' ? 'check' : n.status === 'blocked' ? 'error' : 'clock',
        );
        item.contextValue = `scenarios.comparison.${n.status}`;
        item.accessibilityInformation = { label: `Comparison ${n.label}. ${n.description}` };
        return item;
      }
      case 'table': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.description = n.description;
        item.iconPath = new vscode.ThemeIcon('diff');
        item.contextValue = 'scenarios.table';
        item.tooltip = `${n.label}: ${n.description}\nSearch-time replay on the training instance; not independent validation.`;
        item.accessibilityInformation = {
          label: `Table ${n.label}. ${n.description}. Opens a diff.`,
        };
        item.command = {
          command: 'escurel.scenarios.compare',
          title: 'Compare baseline and candidate',
          arguments: [n.comparison, n.table, n.pageResultSha256],
        };
        return item;
      }
      case 'empty': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('check');
        return item;
      }
      case 'unverified': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('warning');
        return item;
      }
      case 'message': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('info');
        item.command = n.command;
        return item;
      }
    }
  }

  async getChildren(n?: Node): Promise<Node[]> {
    try {
      if (!n) {
        if (!readConfig().evolveEndpoint)
          return [
            {
              kind: 'message',
              label: 'Set escurel.evolveEndpoint to see scenario comparisons.',
              command: {
                command: 'workbench.action.openSettings',
                title: 'Open setting',
                arguments: ['escurel.evolveEndpoint'],
              },
            },
          ];
        const page = await this.client().listInstancesPage({
          skill_id: 'evolve_comparison',
          limit: 100,
        });
        const rows = comparisonPageRows(page.instances);
        this.schedulePoll(rows);
        if (!rows.length)
          return [
            {
              kind: 'message',
              label: 'No comparisons yet. Use “New scenario comparison” above.',
              command: { command: 'escurel.scenarios.new', title: 'New scenario comparison' },
            },
          ];
        return rows;
      }
      if (n.kind !== 'comparison') return [];
      const pageId = `markdown/instances/evolve_comparison/${n.comparison}.md`;
      if (n.status === 'requested')
        return [
          {
            kind: 'message',
            label: 'Waiting to be computed. Open the page and click Compute comparison.',
            command: { command: 'escurel.openPage', title: 'Open page', arguments: [pageId] },
          },
        ];
      if (n.status === 'blocked')
        return [
          {
            kind: 'message',
            label: `Blocked: ${n.reason ?? 'Evolve could not compute this comparison.'}`,
          },
        ];
      if (n.status !== 'completed')
        return [{ kind: 'message', label: 'This page is not a recognizable comparison.' }];
      const { comparison, verified } = await this.load(n.comparison, n.pageResultSha256);
      return tableRows(comparison, verified).map((row) =>
        row.kind === 'table'
          ? { ...row, comparison: n.comparison, pageResultSha256: n.pageResultSha256 }
          : row,
      );
    } catch (e) {
      // A page that says "completed" with no record behind it proves nothing.
      if (e instanceof ComparisonNotFoundError)
        return [
          {
            kind: 'unverified',
            label: 'Unverified: Evolve has no record of this comparison',
            description: '',
          },
        ];
      log().warn(`escurel: scenarios tree: ${describeError(e)}`);
      return [{ kind: 'message', label: describeError(e) }];
    }
  }
}
