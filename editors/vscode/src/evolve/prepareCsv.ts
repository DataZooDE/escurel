import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import { basename, dirname, join } from 'node:path';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { evolveOrigin } from './holdoutClient';
import { preparedV2Draft, preparedV2DraftWithPolicyTerms, validateV2PolicyTerms } from './problemImport';
import { callPrivateTrainingTool } from './trainingCsvClient';
import { requireSavedVisibleFile } from './localFileReview';

function digest(value: Uint8Array): string {
  return createHash('sha256').update(value).digest('hex');
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Evolve returned an invalid source receipt.');
  return value as Record<string, unknown>;
}

export function registerPrepareEvolveTrainingCsv(
  context: vscode.ExtensionContext, services: Services,
): vscode.Disposable {
  const command = vscode.commands.registerCommand('escurel.prepareEvolveTrainingCsv', async () => {
    let preparedSourceId: string | undefined;
    try {
      const owner = await services.subject();
      if (!owner) throw new Error('Sign in before preparing a private training CSV.');
      const endpoint = evolveOrigin(readConfig().evolveEndpoint);
      const sourceId = await vscode.window.showInputBox({
        prompt: 'Unique training source ID (letters, numbers, underscores, hyphens)',
        validateInput: (value) => /^[A-Za-z0-9_-]{1,128}$/.test(value) ? undefined : 'Use 1–128 safe ID characters.',
        ignoreFocusOut: true,
      });
      if (!sourceId) return;
      const read = async (uri: vscode.Uri, label: string, maxBytes: number) => {
        const bytes = await vscode.workspace.fs.readFile(uri);
        if (bytes.length > maxBytes) throw new Error(`${label} exceeds its intake size limit.`);
        const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        const visible = vscode.workspace.textDocuments.find((document) => document.uri.toString() === uri.toString());
        requireSavedVisibleFile(uri.toString(), text, visible ? [{
          uri: visible.uri.toString(), isDirty: visible.isDirty, text: visible.getText(),
        }] : []);
        return { uri, bytes, sha256: digest(bytes), text };
      };
      const choose = async (label: string, extension: string, maxBytes: number) => {
        const uri = (await vscode.window.showOpenDialog({
          canSelectMany: false, openLabel: label, filters: { [extension.toUpperCase()]: [extension] },
        }))?.[0];
        return uri ? read(uri, label, maxBytes) : undefined;
      };
      const active = vscode.window.activeTextEditor?.document;
      const activeCsv = active?.uri.scheme === 'file' && active.uri.fsPath.endsWith('.csv') ? active : undefined;
      const selection = await vscode.window.showQuickPick([
        ...(activeCsv ? [{ label: 'Use active CSV and sibling files',
          description: 'Reads <name>.manifest.json and <name>.template.json beside the saved CSV' }] : []),
        { label: 'Choose three files', description: 'Pick manifest JSON, source template JSON, and dated demand CSV' },
      ], { placeHolder: 'Choose private training CSV files' });
      if (!selection) return;
      const paired = selection.label === 'Use active CSV and sibling files';
      if (paired && activeCsv?.isDirty) throw new Error('Save the active CSV before preparing it.');
      const base = paired ? activeCsv!.uri.fsPath.slice(0, -4) : '';
      const manifest = paired
        ? await read(vscode.Uri.file(join(dirname(base), `${basename(base)}.manifest.json`)),
          'Manifest JSON', 16 * 1024)
        : await choose('Choose manifest JSON', 'json', 16 * 1024);
      if (!manifest) return;
      const template = paired
        ? await read(vscode.Uri.file(join(dirname(base), `${basename(base)}.template.json`)),
          'Source template JSON', 512 * 1024)
        : await choose('Choose source template JSON', 'json', 512 * 1024);
      if (!template) return;
      const csv = paired
        ? await read(activeCsv!.uri, 'Dated training demand CSV', 1024 * 1024)
        : await choose('Choose dated training demand CSV', 'csv', 1024 * 1024);
      if (!csv) return;
      let policy: Awaited<ReturnType<typeof read>> | undefined;
      if (paired) {
        const policyUri = vscode.Uri.file(join(dirname(base), `${basename(base)}.policy.json`));
        try {
          await vscode.workspace.fs.stat(policyUri);
          policy = await read(policyUri, 'Policy terms JSON', 64 * 1024);
        } catch (error) {
          if (!(error instanceof vscode.FileSystemError && error.code === 'FileNotFound')) throw error;
        }
      } else {
        const policyChoice = await vscode.window.showQuickPick([
          { label: 'Choose policy terms JSON', description: 'Merge reviewed SQL, service targets, costs, windows and budget' },
          { label: 'Open incomplete policy draft', description: 'Fill all policy fields before problem import' },
        ], { placeHolder: 'Choose how to author the V2 policy' });
        if (!policyChoice) return;
        if (policyChoice.label === 'Choose policy terms JSON')
          policy = await choose('Choose policy terms JSON', 'json', 64 * 1024);
        if (policyChoice.label === 'Choose policy terms JSON' && !policy) return;
      }
      const policyTerms = policy ? validateV2PolicyTerms(JSON.parse(policy.text)) : undefined;
      const declared = object(JSON.parse(manifest.text));
      const source = object(JSON.parse(template.text));
      const skus = Array.isArray(source.skus) ? source.skus.map(object) : [];
      const csvLines = csv.text.trimEnd().split(/\r?\n/);
      const rowCount = Math.max(0, csvLines.length - 1);
      const scope = `SKUs: ${skus.map((sku) => String(sku.sku_id)).join(', ')}; physical CSV data lines: ${rowCount} (Evolve verifies parsed records)\n`
        + `History: ${String(source.history_start)} to ${String(source.history_end)}; training: ${String(source.training_start)} to ${String(source.training_end)}\n`
        + `Extract cutoff: ${String(declared.extracted_at)}; next-decision cutoff: 12:00 UTC\n`
        + `Opening state: ${skus.map((sku) => `${String(sku.sku_id)} stock=${String(sku.initial_stock)} pipeline=${JSON.stringify(sku.initial_pipeline)}`).join('; ')}`;
      if (declared.daily_demand_sha256 !== csv.sha256)
        throw new Error('Manifest daily_demand_sha256 does not match the selected CSV bytes.');
      const confirmed = await vscode.window.showWarningMessage(
        `Prepare private training source ${sourceId} in Anofox Evolve?`,
        { modal: true, detail: `${scope}\n\nManifest SHA-256: ${manifest.sha256}\nTemplate SHA-256: ${template.sha256}\nCSV SHA-256: ${csv.sha256}${policy ? `\nPolicy terms: ${policy.uri.fsPath}\nPolicy SHA-256: ${policy.sha256}` : '\nPolicy terms: incomplete draft'}\n\nThe raw CSV goes directly to Evolve under your signed-in identity. Derived training demand enters the Escurel problem page if you import the opened draft and may then be read by the planning model. Opening stock and lost-sales estimates remain operator attestations.` },
        'Prepare private CSV',
      );
      if (confirmed !== 'Prepare private CSV') return;
      if (await services.subject() !== owner)
        throw new Error('The signed-in owner changed during review.');
      for (const selected of [manifest, template, csv, ...(policy ? [policy] : [])]) {
        const current = await read(selected.uri, 'Selected training file', selected.bytes.length);
        if (current.sha256 !== selected.sha256)
          throw new Error('A selected file changed after review. Reopen the current files and retry.');
      }
      const receipt = await callPrivateTrainingTool(endpoint, services.auth.refresher, 'evolve_prepare_training_csv', {
        source_id: sourceId, manifest_json: manifest.text,
        template_json: template.text, daily_demand_csv: csv.text,
      });
      if (receipt.training_source_id !== sourceId || typeof receipt.normalized_sha256 !== 'string'
          || !/^[a-f0-9]{64}$/.test(receipt.normalized_sha256))
        throw new Error('Evolve returned an invalid training CSV receipt.');
      preparedSourceId = sourceId;
      const draft = await callPrivateTrainingTool(endpoint, services.auth.refresher, 'evolve_training_csv_draft', { source_id: sourceId });
      if (draft.training_source_id !== sourceId || draft.normalized_sha256 !== receipt.normalized_sha256)
        throw new Error('The owner-private draft differs from the sealed receipt.');
      if (await services.subject() !== owner)
        throw new Error('The signed-in owner changed during preparation.');
      for (const selected of [manifest, template, csv, ...(policy ? [policy] : [])]) {
        const visible = vscode.workspace.textDocuments.find((document) =>
          document.uri.toString() === selected.uri.toString());
        requireSavedVisibleFile(selected.uri.toString(), selected.text,
          visible ? [{ uri: visible.uri.toString(), isDirty: visible.isDirty,
            text: visible.getText() }] : []);
      }
      const document = await vscode.workspace.openTextDocument({
        language: 'json',
        content: JSON.stringify(policyTerms
          ? preparedV2DraftWithPolicyTerms(policyTerms, object(draft.source), sourceId,
            receipt.normalized_sha256 as string)
          : preparedV2Draft({}, object(draft.source), sourceId,
            receipt.normalized_sha256 as string), null, 2) + '\n',
      });
      await vscode.window.showTextDocument(document, { preview: false });
      const action = await vscode.window.showInformationMessage(
        `Training source ${sourceId} prepared from ${String(receipt.row_count)} dated rows. ${policy ? 'Review the merged V2 policy draft' : 'Complete the opened V2 policy draft'}, save it locally, register a matching private holdout, then import the problem.`,
        'Register private holdout',
      );
      if (action === 'Register private holdout') await vscode.commands.executeCommand(
        'escurel.registerEvolveHoldout', {
          sourceId, digest: receipt.normalized_sha256,
          trainingStart: object(draft.source).training_start,
          trainingEnd: object(draft.source).training_end,
        },
      );
    } catch (error) {
      void vscode.window.showErrorMessage(preparedSourceId
        ? `Training source ${preparedSourceId} is sealed. The draft could not be opened: ${describeError(error)}. Correct the local files and retry the same source ID.`
        : `Evolve CSV preparation failed: ${describeError(error)}`);
    }
  });
  context.subscriptions.push(command);
  return command;
}
