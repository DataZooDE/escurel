import * as vscode from 'vscode';
import type { Services } from '../services';
import { readPageMarkdown } from '../fs/read';
import { describeError } from '../errors';
import { smokeOnlyWarnings, v2ProblemPage, v2TrainingStarter } from './problemImport';

export function registerImportEvolveProblem(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const command = vscode.commands.registerCommand('escurel.importEvolveProblem', async () => {
    try {
      const owner = await services.subject();
      if (!owner) throw new Error('Sign in before creating an owner-scoped Evolve problem.');
      const source = await vscode.window.showQuickPick([
        { label: 'Prepare a training source', description: 'Create the private source page and receive its ID and digest' },
        { label: 'Open starter training spec', description: 'Edit it locally, save as JSON, then import it' },
        { label: 'Import completed training spec', description: 'Choose a local JSON file' },
      ], { placeHolder: 'Create an Anofox Evolve V2 problem' });
      if (!source) return;
      if (source.label === 'Prepare a training source') {
        await vscode.commands.executeCommand('escurel.prepareEvolveTrainingSource');
        return;
      }
      if (source.label === 'Open starter training spec') {
        const document = await vscode.workspace.openTextDocument({
          language: 'json', content: JSON.stringify(v2TrainingStarter, null, 2) + '\n',
        });
        await vscode.window.showTextDocument(document, { preview: false });
        return;
      }
      const files = await vscode.window.showOpenDialog({
        canSelectMany: false,
        openLabel: 'Import V2 training spec JSON',
        filters: { JSON: ['json'] },
      });
      const file = files?.[0];
      if (!file) return;
      const bytes = await vscode.workspace.fs.readFile(file);
      const trainingSpec: unknown = JSON.parse(new TextDecoder().decode(bytes));
      const holdoutId = await vscode.window.showInputBox({
        prompt: 'Registered private holdout ID (register it with evolve_register_holdout in chat first)',
        ignoreFocusOut: true,
      });
      if (holdoutId === undefined) return;
      const id = await vscode.window.showInputBox({
        prompt: 'Problem page ID (lowercase letters, numbers, underscores, hyphens)',
        ignoreFocusOut: true,
      });
      if (id === undefined) return;
      const objective = await vscode.window.showInputBox({
        prompt: 'Short objective shown on the problem page',
        ignoreFocusOut: true,
      });
      if (objective === undefined) return;
      const { pageId, content } = v2ProblemPage({
        id: id.trim(), owner, holdoutId: holdoutId.trim(), objective, trainingSpec,
      });
      const client = services.client;
      const existing = await readPageMarkdown(client, pageId);
      if (existing && (existing.skill !== 'evolve_problem' || existing.degraded || !existing.sha256
          || existing.frontmatter.owner_subject !== owner || existing.lastWrittenBy !== owner)) {
        throw new Error('This problem page is unavailable for an owner-authored revision.');
      }
      const spec = trainingSpec as Record<string, unknown>;
      const smokeWarnings = smokeOnlyWarnings(spec);
      if (smokeWarnings.length) {
        const smoke = await vscode.window.showWarningMessage(
          `This is a smoke-only problem: ${smokeWarnings.join(' and ')}. `
          + 'It checks the workflow but does not demonstrate an improved policy. Import it anyway?',
          { modal: true }, 'Import smoke fixture',
        );
        if (smoke !== 'Import smoke fixture') return;
      }
      const confirmed = await vscode.window.showWarningMessage(
        `${existing ? 'Replace the reviewed revision of' : 'Create'} ${id.trim()} for holdout ${holdoutId.trim()}? `
        + `${Array.isArray(spec.skus) ? spec.skus.length : '?'} training SKUs, `
        + `${String(spec.max_generations)} generations. No search starts until a plan is approved.`,
        { modal: true }, 'Import problem',
      );
      if (confirmed !== 'Import problem') return;
      const validation = await client.validate({ content, as_page_id: pageId });
      if (!validation.ok || validation.issues.some((issue) => issue.severity === 'error')) {
        const issues = validation.issues.map((issue) => issue.message).join('; ');
        throw new Error(`Escurel rejected this problem page: ${issues || 'unknown schema issue'}`);
      }
      const written = await client.updatePage({
        page_id: pageId,
        content,
        base_sha256: existing?.sha256 ?? '',
      });
      if (!written.ok) {
        throw new Error(`Escurel rejected this problem page: ${written.issues.map((issue) => issue.message).join('; ')}`);
      }
      services.onDidChangeEmit();
      await vscode.commands.executeCommand('escurel.openPage', pageId);
      void vscode.window.showInformationMessage(
        'V2 problem saved. Open the page and choose Review experiment plan; Workbench checks the registered private holdout first.',
      );
    } catch (error) {
      void vscode.window.showErrorMessage(`Evolve problem import failed: ${describeError(error)}`);
    }
  });
  context.subscriptions.push(command);
  return command;
}
