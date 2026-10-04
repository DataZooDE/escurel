import * as vscode from 'vscode';
import type { RunnerTree } from './runner';
import { filterFromPicks, filterPickItems, type RunsNode } from './runsModel';

const asNode = (arg: unknown): RunsNode | undefined =>
  arg && typeof arg === 'object' && 'runId' in arg ? (arg as RunsNode) : undefined;

/** The commands that work on the runs panel's rows and header. */
export function registerRunsCommands(tree: RunnerTree): vscode.Disposable {
  return vscode.Disposable.from(
    vscode.commands.registerCommand('escurel.runs.refresh', () => tree.refresh()),
    vscode.commands.registerCommand('escurel.runs.loadMore', () => tree.loadMore()),
    vscode.commands.registerCommand('escurel.runs.clearFilter', () => tree.setFilter({})),
    vscode.commands.registerCommand('escurel.runs.filter', async () => {
      const qp = vscode.window.createQuickPick<vscode.QuickPickItem & { id: string }>();
      qp.title = 'Filter runs';
      qp.placeholder = 'Pick what to show in History (nothing picked shows everything)';
      qp.canSelectMany = true;
      const items = filterPickItems(tree.knownSkills(), tree.getFilter()).map(
        ({ id, label, description, picked }) => ({
          id,
          label,
          ...(description ? { description } : {}),
          picked,
        }),
      );
      qp.items = items;
      qp.selectedItems = items.filter((i) => i.picked);
      const picks = await new Promise<readonly string[] | undefined>((resolve) => {
        qp.onDidAccept(() => {
          resolve(qp.selectedItems.map((i) => i.id));
          qp.hide();
        });
        qp.onDidHide(() => resolve(undefined));
        qp.show();
      });
      qp.dispose();
      if (!picks) return;
      let text: string | undefined;
      if (picks.includes('text')) {
        text = await vscode.window.showInputBox({
          prompt: 'Show runs whose page, reason or result contains…',
          value: tree.getFilter().text ?? '',
        });
        if (text === undefined) return;
      }
      await tree.setFilter(filterFromPicks(picks, text));
    }),
    vscode.commands.registerCommand('escurel.runs.openThread', async (arg?: unknown) => {
      const node = asNode(arg);
      if (!node?.rootEventId) {
        void vscode.window.showInformationMessage('This run has no thread to open.');
        return;
      }
      await vscode.commands.executeCommand('escurel.openThread', node.rootEventId);
    }),
    vscode.commands.registerCommand('escurel.runs.openTarget', async (arg?: unknown) => {
      const node = asNode(arg);
      if (!node?.pageId) {
        void vscode.window.showInformationMessage('This run did not work on a page.');
        return;
      }
      await vscode.commands.executeCommand('escurel.openPage', node.pageId);
    }),
    vscode.commands.registerCommand('escurel.runs.copyRunId', async (arg?: unknown) => {
      const node = asNode(arg);
      if (!node?.runId) return;
      await vscode.env.clipboard.writeText(node.runId);
      vscode.window.setStatusBarMessage('Run id copied', 2500);
    }),
  );
}
