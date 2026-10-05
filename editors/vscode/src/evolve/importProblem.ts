import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readPageMarkdown } from '../fs/read';
import { describeError } from '../errors';
import { smokeOnlyWarnings, v2ProblemPage, v2TrainingStarter } from './problemImport';
import { savedHoldout } from './registerHoldout';
import { evolveOrigin } from './holdoutClient';
import { readConfig } from '../config';

export function registerImportEvolveProblem(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const command = vscode.commands.registerCommand('escurel.importEvolveProblem', async () => {
    try {
      const owner = await services.subject();
      if (!owner) throw new Error('Sign in before creating an owner-scoped Evolve problem.');
      const active = vscode.window.activeTextEditor?.document;
      const activeJson = active?.uri.scheme === 'file' && active.uri.fsPath.endsWith('.json') ? active : undefined;
      const source = await vscode.window.showQuickPick([
        { label: 'Prepare a training source', description: 'Create the private source page and receive its ID and digest' },
        { label: 'Register private holdout', description: 'Submit a local V2 holdout file directly to Anofox Evolve' },
        { label: 'Open starter training spec', description: 'Edit it locally, save as JSON, then import it' },
        ...(activeJson ? [{ label: 'Import active completed training spec', description: activeJson.uri.fsPath }] : []),
        { label: 'Import completed training spec', description: 'Choose a local JSON file' },
      ], { placeHolder: 'Create an Anofox Evolve V2 problem' });
      if (!source) return;
      if (source.label === 'Prepare a training source') {
        await vscode.commands.executeCommand('escurel.prepareEvolveTrainingSource');
        return;
      }
      if (source.label === 'Register private holdout') {
        await vscode.commands.executeCommand('escurel.registerEvolveHoldout');
        return;
      }
      if (source.label === 'Open starter training spec') {
        const document = await vscode.workspace.openTextDocument({
          language: 'json', content: JSON.stringify(v2TrainingStarter, null, 2) + '\n',
        });
        await vscode.window.showTextDocument(document, { preview: false });
        return;
      }
      const selectedActive = source.label === 'Import active completed training spec';
      const file = selectedActive ? activeJson!.uri : (await vscode.window.showOpenDialog({
        canSelectMany: false, openLabel: 'Import V2 training spec JSON', filters: { JSON: ['json'] },
      }))?.[0];
      if (!file) return;
      const bytes = await vscode.workspace.fs.readFile(file);
      const fileSha256 = createHash('sha256').update(bytes).digest('hex');
      const visible = vscode.workspace.textDocuments.find((document) => document.uri.toString() === file.toString());
      if (visible && (visible.isDirty || visible.getText() !== new TextDecoder().decode(bytes)))
        throw new Error('The selected training spec has unsaved or stale editor content. Save and review it before import.');
      const trainingSpec: unknown = JSON.parse(new TextDecoder().decode(bytes));
      const specForLookup = trainingSpec as Record<string, unknown>;
      let recent: string | undefined;
      try {
        const endpoint = evolveOrigin(readConfig().evolveEndpoint);
        if (typeof specForLookup.training_source_id === 'string'
            && typeof specForLookup.source_sha256 === 'string') {
          recent = savedHoldout(context, owner, endpoint,
            specForLookup.training_source_id, specForLookup.source_sha256)?.holdoutId;
        }
      } catch { /* A configured endpoint is required for direct registration, not chat imports. */ }
      const holdoutId = await vscode.window.showInputBox({
        prompt: 'Registered private holdout ID (Workbench registration or Evolve chat tool)',
        value: recent,
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
      if (await services.subject() !== owner)
        throw new Error('The signed-in owner changed during review. Sign in again before importing.');
      const current = await vscode.workspace.fs.readFile(file);
      if (createHash('sha256').update(current).digest('hex') !== fileSha256)
        throw new Error('The selected training spec changed during review. Reopen and review the current file.');
      if (visible && (visible.isDirty || visible.getText() !== new TextDecoder().decode(current)))
        throw new Error('The selected training spec changed in the editor during review. Save and review it again.');
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
