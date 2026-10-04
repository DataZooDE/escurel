import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { evolveOrigin } from './holdoutClient';
import { preparedV2Draft } from './problemImport';

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
      const choose = async (label: string, extension: string, maxBytes: number) => {
        const uri = (await vscode.window.showOpenDialog({
          canSelectMany: false, openLabel: label, filters: { [extension.toUpperCase()]: [extension] },
        }))?.[0];
        if (!uri) return undefined;
        const bytes = await vscode.workspace.fs.readFile(uri);
        if (bytes.length > maxBytes) throw new Error(`${label} exceeds its intake size limit.`);
        return { uri, bytes, sha256: digest(bytes), text: new TextDecoder('utf-8', { fatal: true }).decode(bytes) };
      };
      const manifest = await choose('Choose manifest JSON', 'json', 16 * 1024);
      if (!manifest) return;
      const template = await choose('Choose source template JSON', 'json', 512 * 1024);
      if (!template) return;
      const csv = await choose('Choose dated training demand CSV', 'csv', 1024 * 1024);
      if (!csv) return;
      const declared = object(JSON.parse(manifest.text));
      const source = object(JSON.parse(template.text));
      const skus = Array.isArray(source.skus) ? source.skus.map(object) : [];
      const csvLines = csv.text.trimEnd().split(/\r?\n/);
      const rowCount = Math.max(0, csvLines.length - 1);
      const estimateRows = csvLines.slice(1).filter((line) => line.split(',')[4] === 'true').length;
      const scope = `SKUs: ${skus.map((sku) => String(sku.sku_id)).join(', ')}; dated rows: ${rowCount}; stockout estimate rows: ${estimateRows}\n`
        + `History: ${String(source.history_start)} to ${String(source.history_end)}; training: ${String(source.training_start)} to ${String(source.training_end)}\n`
        + `Extract cutoff: ${String(declared.extracted_at)}; next-decision cutoff: 12:00 UTC\n`
        + `Opening state: ${skus.map((sku) => `${String(sku.sku_id)} stock=${String(sku.initial_stock)} pipeline=${JSON.stringify(sku.initial_pipeline)}`).join('; ')}`;
      if (declared.daily_demand_sha256 !== csv.sha256)
        throw new Error('Manifest daily_demand_sha256 does not match the selected CSV bytes.');
      const confirmed = await vscode.window.showWarningMessage(
        `Prepare private training source ${sourceId} in Anofox Evolve?`,
        { modal: true, detail: `${scope}\n\nManifest SHA-256: ${manifest.sha256}\nTemplate SHA-256: ${template.sha256}\nCSV SHA-256: ${csv.sha256}\n\nThe CSV goes directly to Evolve under your signed-in identity. Its dated rows do not enter an Escurel page, event, or chat message. Opening stock and lost-sales estimates remain operator attestations.` },
        'Prepare private CSV',
      );
      if (confirmed !== 'Prepare private CSV') return;
      if (await services.subject() !== owner)
        throw new Error('The signed-in owner changed during review.');
      for (const selected of [manifest, template, csv]) {
        if (digest(await vscode.workspace.fs.readFile(selected.uri)) !== selected.sha256)
          throw new Error('A selected file changed after review. Reopen the current files and retry.');
      }
      const call = async (tool: string, body: Record<string, unknown>) => {
        const token = await services.auth.refresher.get();
        if (!token) throw new Error('Sign in with an OIDC token accepted by Evolve.');
        const response = await fetch(endpoint + '/', {
          method: 'POST', redirect: 'error', signal: AbortSignal.timeout(30_000),
          headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', 'X-Triton-Tool': tool },
          body: JSON.stringify(body),
        });
        if (response.status === 409)
          throw new Error('This source ID is already frozen with different files. Use a new ID for the changed extract.');
        if (response.status === 403)
          throw new Error('This source belongs to another signed-in owner. Use the original owner or a new source ID.');
        if (response.status === 422)
          throw new Error('Evolve rejected the CSV schema or measurements. Fix the selected files and retry the same ID.');
        if (response.status === 401)
          throw new Error('Evolve rejected the OIDC token. Sign in again and check its audience.');
        if (!response.ok) throw new Error(`Evolve rejected ${tool} (HTTP ${response.status}). Retry the same files and ID if the result is uncertain.`);
        return object(await response.json());
      };
      const receipt = await call('evolve_prepare_training_csv', {
        source_id: sourceId, manifest_json: manifest.text,
        template_json: template.text, daily_demand_csv: csv.text,
      });
      if (receipt.training_source_id !== sourceId || typeof receipt.normalized_sha256 !== 'string'
          || !/^[a-f0-9]{64}$/.test(receipt.normalized_sha256))
        throw new Error('Evolve returned an invalid training CSV receipt.');
      const draft = await call('evolve_training_csv_draft', { source_id: sourceId });
      if (draft.training_source_id !== sourceId || draft.normalized_sha256 !== receipt.normalized_sha256)
        throw new Error('The owner-private draft differs from the sealed receipt.');
      if (await services.subject() !== owner)
        throw new Error('The signed-in owner changed during preparation.');
      const document = await vscode.workspace.openTextDocument({
        language: 'json',
        content: JSON.stringify(preparedV2Draft({}, object(draft.source), sourceId,
          receipt.normalized_sha256 as string), null, 2) + '\n',
      });
      await vscode.window.showTextDocument(document, { preview: false });
      const action = await vscode.window.showInformationMessage(
        `Training source ${sourceId} prepared from ${String(receipt.row_count)} dated rows. Complete the opened V2 policy draft, save it locally, register a matching private holdout, then import the problem.`,
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
      void vscode.window.showErrorMessage(`Evolve CSV preparation failed: ${describeError(error)}`);
    }
  });
  context.subscriptions.push(command);
  return command;
}
