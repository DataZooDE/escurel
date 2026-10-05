import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import {
  EvolveRegistrationError,
  evolveOrigin,
  prepareHoldoutCsv,
  registerHoldoutCsvAtEvolve,
  type PreparedSourceBinding,
  type RegisteredHoldout,
} from './holdoutClient';

const registrationKey = 'evolve.registeredHoldouts';
const pendingKey = 'evolve.pendingHoldoutCsvRegistrations';

interface SelectedFile {
  uri: vscode.Uri;
  bytes: Uint8Array;
  text: string;
  sha256: string;
}

export function registerEvolveHoldoutCsv(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const command = vscode.commands.registerCommand(
    'escurel.registerEvolveHoldoutCsv',
    async (prepared?: PreparedSourceBinding) => {
      try {
        const owner = await services.subject();
        if (!owner) throw new Error('Sign in before registering private outcomes.');
        const endpoint = evolveOrigin(readConfig().evolveEndpoint);
        const read = async (
          uri: vscode.Uri,
          label: string,
          limit: number,
        ): Promise<SelectedFile> => {
          const bytes = await vscode.workspace.fs.readFile(uri);
          if (bytes.length > limit) throw new Error(`${label} exceeds its intake size limit.`);
          if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf)
            throw new Error(
              `${label} has a UTF-8 BOM. Remove the BOM so the reviewed bytes match the submitted digest.`,
            );
          let text: string;
          try {
            text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
          } catch {
            throw new Error(`${label} must be valid UTF-8.`);
          }
          const open = vscode.workspace.textDocuments.find(
            (document) => document.uri.toString() === uri.toString(),
          );
          if (open && (open.isDirty || open.getText() !== text))
            throw new Error(`Save ${label} and review its current contents before registering.`);
          return { uri, bytes, text, sha256: createHash('sha256').update(bytes).digest('hex') };
        };
        const pick = async (label: string, ext: 'json' | 'csv', limit: number) => {
          const uri = (
            await vscode.window.showOpenDialog({
              canSelectMany: false,
              openLabel: label,
              filters: { [ext.toUpperCase()]: [ext] },
            })
          )?.[0];
          return uri ? read(uri, label, limit) : undefined;
        };
        const manifest = await pick('Choose holdout manifest JSON', 'json', 16_384);
        if (!manifest) return;
        const template = await pick('Choose metadata-only holdout template JSON', 'json', 524_288);
        if (!template) return;
        const csv = await pick('Choose dated holdout demand CSV', 'csv', 1_048_576);
        if (!csv) return;
        let manifestValue: Record<string, unknown>;
        let templateValue: unknown;
        try {
          manifestValue = JSON.parse(manifest.text) as Record<string, unknown>;
          templateValue = JSON.parse(template.text) as unknown;
        } catch {
          throw new Error('Manifest and template must be valid JSON.');
        }
        if (
          manifestValue.format_version !== 'holdout_demand_csv_v1' ||
          manifestValue.daily_demand_sha256 !== csv.sha256
        )
          throw new Error(
            'Manifest format or daily_demand_sha256 does not match the selected CSV bytes.',
          );
        const sourceId =
          prepared?.sourceId ??
          (await vscode.window.showInputBox({
            prompt: 'Registered Evolve training-source ID',
            ignoreFocusOut: true,
          }));
        if (sourceId === undefined) return;
        const digest =
          prepared?.digest ??
          (await vscode.window.showInputBox({
            prompt: 'Server-computed normalized training-source SHA-256',
            ignoreFocusOut: true,
          }));
        if (digest === undefined) return;
        const {
          template: checked,
          templateJson,
          summary,
        } = prepareHoldoutCsv(templateValue, {
          sourceId: sourceId.trim(),
          digest: digest.trim(),
          trainingStart: prepared?.trainingStart,
          trainingEnd: prepared?.trainingEnd,
        });
        const holdoutId = String(checked.holdout_id);
        const submittedTemplateSha256 = createHash('sha256').update(templateJson).digest('hex');
        const confirmed = await vscode.window.showWarningMessage(
          `Seal dated CSV holdout ${holdoutId} in Anofox Evolve?`,
          {
            modal: true,
            detail: `${summary}\nManifest SHA-256: ${manifest.sha256}\nSelected template SHA-256: ${template.sha256}\nSubmitted template SHA-256 (prepared source binding applied): ${submittedTemplateSha256}\nDemand CSV SHA-256: ${csv.sha256}\nExtract: ${String(manifestValue.extract_id)} at ${String(manifestValue.extracted_at)}\n\nThe demand CSV is sent directly to Evolve under your signed-in identity. Its outcome values are not displayed in this dialog or sent through Escurel chat.`,
          },
          'Seal private CSV holdout',
        );
        if (confirmed !== 'Seal private CSV holdout') return;
        if ((await services.subject()) !== owner)
          throw new Error(
            'The signed-in owner changed during review. Sign in again and repeat registration.',
          );
        for (const selected of [manifest, template, csv]) {
          const current = await vscode.workspace.fs.readFile(selected.uri);
          if (createHash('sha256').update(current).digest('hex') !== selected.sha256)
            throw new Error(
              'A selected file changed during review. Reopen and review the current files.',
            );
          const open = vscode.workspace.textDocuments.find(
            (document) => document.uri.toString() === selected.uri.toString(),
          );
          if (open && (open.isDirty || open.getText() !== selected.text))
            throw new Error(
              'A selected file has unsaved editor changes. Save and review the current contents.',
            );
        }
        type Pending = {
          owner: string;
          endpoint: string;
          holdoutId: string;
          requestSha256: string;
        };
        const body = {
          manifest_json: manifest.text,
          template_json: templateJson,
          daily_demand_csv: csv.text,
        };
        const requestDigest = createHash('sha256').update(JSON.stringify(body)).digest('hex');
        const pending = context.workspaceState.get<Pending[]>(pendingKey, []);
        const prior = pending.find(
          (entry) =>
            entry.owner === owner && entry.endpoint === endpoint && entry.holdoutId === holdoutId,
        );
        if (prior && prior.requestSha256 !== requestDigest)
          throw new Error(
            'A prior upload of this holdout ID has an unknown outcome and different files. Restore the reviewed files or choose a new ID.',
          );
        await context.workspaceState.update(
          pendingKey,
          [
            { owner, endpoint, holdoutId, requestSha256: requestDigest },
            ...pending.filter(
              (entry) =>
                entry.owner !== owner ||
                entry.endpoint !== endpoint ||
                entry.holdoutId !== holdoutId,
            ),
          ].slice(0, 20),
        );
        let receipt;
        try {
          receipt = await registerHoldoutCsvAtEvolve(endpoint, services.auth.refresher, body);
        } catch (error) {
          if (error instanceof EvolveRegistrationError && error.conclusiveNoStore)
            await context.workspaceState.update(
              pendingKey,
              context.workspaceState
                .get<Pending[]>(pendingKey, [])
                .filter(
                  (entry) =>
                    entry.owner !== owner ||
                    entry.endpoint !== endpoint ||
                    entry.holdoutId !== holdoutId,
                ),
            );
          throw error;
        }
        if ((await services.subject()) !== owner)
          throw new Error(
            'The signed-in owner changed during upload. Check Evolve under the registering account before retrying.',
          );
        await context.workspaceState.update(
          pendingKey,
          context.workspaceState
            .get<Pending[]>(pendingKey, [])
            .filter(
              (entry) =>
                entry.owner !== owner ||
                entry.endpoint !== endpoint ||
                entry.holdoutId !== holdoutId,
            ),
        );
        const record: RegisteredHoldout = {
          holdoutId: receipt.holdoutId,
          holdoutSha256: receipt.holdoutSha256,
          trainingSourceId: sourceId.trim(),
          trainingSourceSha256: digest.trim(),
          owner,
          endpoint,
        };
        const saved = context.workspaceState.get<RegisteredHoldout[]>(registrationKey, []);
        await context.workspaceState.update(
          registrationKey,
          [
            record,
            ...saved.filter(
              (entry) =>
                entry.owner !== owner ||
                entry.endpoint !== endpoint ||
                entry.trainingSourceId !== record.trainingSourceId ||
                entry.trainingSourceSha256 !== record.trainingSourceSha256,
            ),
          ].slice(0, 20),
        );
        const action = await vscode.window.showInformationMessage(
          `Private CSV holdout ${receipt.holdoutId} sealed in Evolve. Declaration SHA-256: ${receipt.holdoutSha256}. Registration records submitted evidence; it does not independently validate unseen outcomes or operational performance.`,
          'Import V2 problem',
        );
        if (action === 'Import V2 problem')
          await vscode.commands.executeCommand('escurel.importEvolveProblem');
      } catch (error) {
        void vscode.window.showErrorMessage(
          `Evolve CSV holdout registration failed: ${describeError(error)}`,
        );
      }
    },
  );
  context.subscriptions.push(command);
  return command;
}
