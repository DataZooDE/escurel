import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { log } from '../log';
import type { Services } from '../services';
import {
  fetchScenarioDiff,
  scenarioDiffSummary,
  scenarioDiffTexts,
  type ScenarioDiff,
} from '../evolve/scenarioDiff';
import {
  SCENARIO_SCHEME,
  experimentRows,
  parseScenarioUri,
  scenarioUri,
  tableRows,
  type EmptyRow,
  type ExperimentRow,
  type TableRow,
} from './scenariosModel';

interface MessageRow {
  kind: 'message';
  label: string;
  command?: vscode.Command;
}

type Node =
  | (ExperimentRow & { experimentId: string })
  | (TableRow & { experiment: string })
  | EmptyRow
  | MessageRow;

/**
 * Scenarios: what an Evolve winner changed against its seed, per state table.
 *
 * Experiments come from Escurel (so the usual owner ACL applies); the diff itself is read from
 * Evolve over the signed-in token, the same direct path the holdout commands use. Opening a table
 * shows the seed and the winner side by side in the native diff editor. This view only reads: it
 * never applies a scenario to anything.
 */
export class ScenariosTree
  implements vscode.TreeDataProvider<Node>, vscode.TextDocumentContentProvider
{
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly contentChanged = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this.contentChanged.event;
  private readonly diffs = new Map<string, Promise<ScenarioDiff>>();

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
    context.subscriptions.push(
      vscode.window.createTreeView('escurel.scenarios', { treeDataProvider: tree }),
      vscode.workspace.registerTextDocumentContentProvider(SCENARIO_SCHEME, tree),
      vscode.commands.registerCommand('escurel.scenarios.refresh', () => tree.refresh()),
      vscode.commands.registerCommand(
        'escurel.scenarios.compare',
        (experiment: string, table: string) => tree.compare(experiment, table),
      ),
    );
    return tree;
  }

  refresh(): void {
    this.diffs.clear();
    this.changed.fire(undefined);
  }

  private diff(experiment: string): Promise<ScenarioDiff> {
    let pending = this.diffs.get(experiment);
    if (!pending) {
      pending = fetchScenarioDiff(
        readConfig().evolveEndpoint,
        this.services.auth.refresher,
        experiment,
      );
      // A failure must not be cached: the next click retries.
      pending.catch(() => this.diffs.delete(experiment));
      this.diffs.set(experiment, pending);
    }
    return pending;
  }

  async compare(experiment: string, table: string): Promise<void> {
    try {
      const diff = await this.diff(experiment);
      const lines = scenarioDiffSummary(diff);
      const seed = vscode.Uri.parse(scenarioUri(experiment, table, 'seed'));
      const winner = vscode.Uri.parse(scenarioUri(experiment, table, 'winner'));
      await vscode.commands.executeCommand(
        'vscode.diff',
        seed,
        winner,
        `${experiment} · ${table}: seed ↔ winner`,
      );
      void vscode.window.setStatusBarMessage(lines[lines.length - 1] ?? '', 8000);
    } catch (e) {
      void vscode.window.showErrorMessage(describeError(e));
    }
  }

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const parsed = parseScenarioUri(uri.toString());
    if (!parsed) return '';
    const texts = scenarioDiffTexts(await this.diff(parsed.experiment), parsed.table);
    return parsed.side === 'seed' ? texts.seed : texts.winner;
  }

  getTreeItem(n: Node): vscode.TreeItem {
    switch (n.kind) {
      case 'experiment': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.Collapsed);
        item.description = n.description;
        item.iconPath = new vscode.ThemeIcon('beaker');
        item.contextValue = 'scenarios.experiment';
        item.accessibilityInformation = { label: `Experiment ${n.label}. ${n.description}` };
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
          title: 'Compare seed and winner',
          arguments: [n.experiment, n.table],
        };
        return item;
      }
      case 'empty': {
        const item = new vscode.TreeItem(n.label, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('check');
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
              label: 'Set escurel.evolveEndpoint to see scenario diffs.',
              command: {
                command: 'workbench.action.openSettings',
                title: 'Open setting',
                arguments: ['escurel.evolveEndpoint'],
              },
            },
          ];
        const page = await this.client().listInstancesPage({
          skill_id: 'evolve_experiment',
          limit: 100,
        });
        const rows = experimentRows(page.instances);
        if (!rows.length) return [{ kind: 'message', label: 'No Evolve experiments yet.' }];
        return rows.map((row) => ({ ...row, experimentId: row.experiment }));
      }
      if (n.kind === 'experiment') {
        const diff = await this.diff(n.experimentId);
        return tableRows(diff).map((row) =>
          row.kind === 'table' ? { ...row, experiment: n.experimentId } : row,
        );
      }
      return [];
    } catch (e) {
      log().warn(`escurel: scenarios tree: ${describeError(e)}`);
      return [{ kind: 'message', label: describeError(e) }];
    }
  }
}
