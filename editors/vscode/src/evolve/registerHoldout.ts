import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import {
  EvolveRegistrationError,
  evolveOrigin,
  prepareHoldout,
  registerHoldoutAtEvolve,
  requireReviewedFileBytes,
  type PreparedSourceBinding,
  type RegisteredHoldout,
} from './holdoutClient';

const registrationKey = 'evolve.registeredHoldouts';
const pendingKey = 'evolve.pendingHoldoutRegistrations';

export function savedHoldout(
  context: vscode.ExtensionContext,
  owner: string,
  endpoint: string,
  sourceId: string,
  digest: string,
): RegisteredHoldout | undefined {
  const saved = context.workspaceState.get<RegisteredHoldout[]>(registrationKey, []);
  return saved.find(
    (entry) =>
      entry.owner === owner &&
      entry.endpoint === endpoint &&
      entry.trainingSourceId === sourceId &&
      entry.trainingSourceSha256 === digest,
  );
}

export function registerEvolveHoldout(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const command = vscode.commands.registerCommand(
    'escurel.registerEvolveHoldout',
    async (prepared?: PreparedSourceBinding) => {
      try {
        const format = await vscode.window.showQuickPick(
          [
            {
              label: 'Full JSON declaration',
              description: 'A private V2 declaration with outcome arrays',
            },
            {
              label: 'Metadata template plus dated CSV',
              description: 'Keep outcome rows in a separate CSV sent directly to Evolve',
            },
          ],
          { placeHolder: 'Choose private holdout format' },
        );
        if (!format) return;
        if (format.label === 'Metadata template plus dated CSV') {
          await vscode.commands.executeCommand('escurel.registerEvolveHoldoutCsv', prepared);
          return;
        }
        const owner = await services.subject();
        if (!owner) throw new Error('Sign in before registering private outcomes.');
        const endpoint = evolveOrigin(readConfig().evolveEndpoint);
        const active = vscode.window.activeTextEditor?.document;
        const choices = [
          ...(active?.uri.scheme === 'file' && active.uri.path.endsWith('.json')
            ? [{ label: 'Use active JSON file', description: active.uri.fsPath }]
            : []),
          {
            label: 'Choose local JSON file',
            description: 'Read the selected file in the extension host',
          },
        ];
        const selected = await vscode.window.showQuickPick(choices, {
          placeHolder: 'Choose the private V2 holdout declaration',
        });
        if (!selected) return;
        const file =
          selected.label === 'Use active JSON file'
            ? active!.uri
            : (
                await vscode.window.showOpenDialog({
                  canSelectMany: false,
                  openLabel: 'Choose private V2 holdout JSON',
                  filters: { JSON: ['json'] },
                })
              )?.[0];
        if (!file) return;
        const bytes = await vscode.workspace.fs.readFile(file);
        if (selected.label === 'Use active JSON file')
          requireReviewedFileBytes(bytes, active!.getText(), active!.isDirty);
        if (bytes.length > 2_000_000)
          throw new Error('Private holdout JSON exceeds the 2 MB intake limit.');
        let input: unknown;
        try {
          input = JSON.parse(new TextDecoder().decode(bytes));
        } catch {
          throw new Error('The selected private holdout file is not valid JSON.');
        }
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
        const { payload, summary } = prepareHoldout(input, {
          sourceId: sourceId.trim(),
          digest: digest.trim(),
          trainingStart: prepared?.trainingStart,
          trainingEnd: prepared?.trainingEnd,
        });
        const confirmed = await vscode.window.showWarningMessage(
          `Seal private V2 holdout ${String(payload.holdout_id)} in Anofox Evolve?`,
          {
            modal: true,
            detail: `${summary}\n\nThe local outcome rows go directly to Evolve under your signed-in identity. Escurel receives only the holdout ID in the later problem page.`,
          },
          'Seal private holdout',
        );
        if (confirmed !== 'Seal private holdout') return;
        if ((await services.subject()) !== owner)
          throw new Error(
            'The signed-in owner changed during review. Sign in again and repeat registration.',
          );
        const current = await vscode.workspace.fs.readFile(file);
        if (
          createHash('sha256').update(current).digest('hex') !==
          createHash('sha256').update(bytes).digest('hex')
        )
          throw new Error(
            'The private holdout file changed during review. Reopen and review the current bytes.',
          );
        if (selected.label === 'Use active JSON file')
          requireReviewedFileBytes(current, active!.getText(), active!.isDirty);
        const fileSha256 = createHash('sha256').update(bytes).digest('hex');
        const payloadSha256 = createHash('sha256').update(JSON.stringify(payload)).digest('hex');
        type Pending = {
          owner: string;
          endpoint: string;
          holdoutId: string;
          fileUri: string;
          fileSha256: string;
          payloadSha256: string;
        };
        const pending = context.workspaceState.get<Pending[]>(pendingKey, []);
        const prior = pending.find(
          (entry) =>
            entry.owner === owner &&
            entry.endpoint === endpoint &&
            entry.holdoutId === payload.holdout_id,
        );
        if (prior && (prior.fileSha256 !== fileSha256 || prior.payloadSha256 !== payloadSha256))
          throw new Error(
            'A prior upload of this holdout ID has an unknown outcome and a different declaration. Restore the reviewed file and source binding, or choose a new ID.',
          );
        await context.workspaceState.update(
          pendingKey,
          [
            {
              owner,
              endpoint,
              holdoutId: String(payload.holdout_id),
              fileUri: file.toString(),
              fileSha256,
              payloadSha256,
            },
            ...pending.filter(
              (entry) =>
                entry.owner !== owner ||
                entry.endpoint !== endpoint ||
                entry.holdoutId !== payload.holdout_id,
            ),
          ].slice(0, 20),
        );
        let receipt;
        try {
          receipt = await registerHoldoutAtEvolve(endpoint, services.auth.refresher, payload);
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
                    entry.holdoutId !== payload.holdout_id,
                ),
            );
          throw error;
        }
        if ((await services.subject()) !== owner)
          throw new Error(
            'The signed-in owner changed during upload. Check the Evolve registration under the current account before retrying.',
          );
        await context.workspaceState.update(
          pendingKey,
          context.workspaceState
            .get<Pending[]>(pendingKey, [])
            .filter(
              (entry) =>
                entry.owner !== owner ||
                entry.endpoint !== endpoint ||
                entry.holdoutId !== payload.holdout_id,
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
          `Private holdout ${receipt.holdoutId} sealed in Evolve. Declaration SHA-256: ${receipt.holdoutSha256}. This records the registered declaration and operator attestations; it does not establish unseen outcomes or operational validity.`,
          'Import V2 problem',
        );
        if (action === 'Import V2 problem')
          await vscode.commands.executeCommand('escurel.importEvolveProblem');
      } catch (error) {
        void vscode.window.showErrorMessage(
          `Evolve holdout registration failed: ${describeError(error)}`,
        );
      }
    },
  );
  context.subscriptions.push(command);
  return command;
}
