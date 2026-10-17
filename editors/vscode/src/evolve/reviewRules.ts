import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { evolveOrigin } from './holdoutClient';
import { pickJsonObject, type JsonFileHost } from './jsonObjectFile';
import { readStateInstance } from './stateInstance';
import {
  approveRules,
  getRuleBatch,
  proposeRules,
  startWithApprovedRules,
  testRules,
  type RulesInput,
} from './rulesClient';
import {
  confirmTestDetail,
  ruleChoices,
  reviewRules,
  startWithRules,
  type RulesApi,
  type RulesUi,
} from './rulesReview';
import {
  ALPHA_CAP,
  P_MAX_CAP,
  describePolicy,
  parsePolicyInput,
  percent,
  type PolicyChoice,
} from './rulesModel';

/** VS Code as the host of the JSON-file picker: open saved editors, or a file the person chooses. */
const jsonFileHost: JsonFileHost<vscode.Uri> = {
  openJsonDocs: () =>
    vscode.workspace.textDocuments
      .filter((doc) => doc.uri.scheme === 'file' && doc.uri.path.endsWith('.json'))
      .map((doc) => ({
        uri: doc.uri,
        name: doc.uri.path.split('/').pop() ?? '',
        fsPath: doc.uri.fsPath,
        isDirty: doc.isDirty,
        getText: () => doc.getText(),
      })),
  pick: async (choices, placeHolder) => vscode.window.showQuickPick(choices, { placeHolder }),
  chooseFile: async (openLabel) =>
    (
      await vscode.window.showOpenDialog({
        canSelectMany: false,
        openLabel,
        filters: { JSON: ['json'] },
      })
    )?.[0],
  read: async (uri) => vscode.workspace.fs.readFile(uri),
};

/** The person's own start file: an open (saved) JSON editor, or one they choose. */
const readStartSpec = () =>
  pickJsonObject(jsonFileHost, {
    what: 'start file',
    placeHolder: 'Choose the new replenishment experiment start file',
    chooseDescription: 'The new experiment\u2019s start request',
    openLabel: 'Choose start file',
  });

/** The instance of a CSV-start pilot (P0, P5, P7), read once and checked against the pilot's keys. */
async function readInstance(): Promise<Record<string, unknown> | undefined> {
  const raw = await pickJsonObject(jsonFileHost, {
    what: 'instance file',
    placeHolder: 'Choose the P0, P5 or P7 instance file (the start request of the experiment)',
    chooseDescription: 'Pilot p0, p5 or p7 with its capacity and CSV tables',
    openLabel: 'Choose instance file',
  });
  return raw ? readStateInstance(raw) : undefined;
}

/** The report is shown read-only: a virtual document, so it can neither be edited nor left "unsaved". */
const START_LABEL = 'Start new experiment with these rules';
const REPORT_SCHEME = 'escurel-evolve-rules';

export function registerReviewEvolveRules(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const reports = new Map<string, string>();
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(REPORT_SCHEME, {
      provideTextDocumentContent: (uri) => reports.get(uri.path) ?? '',
    }),
  );
  const command = vscode.commands.registerCommand(
    'escurel.reviewEvolveRules',
    async (prepared?: { sourceId?: string }) => {
      try {
        const endpoint = evolveOrigin(readConfig().evolveEndpoint);
        const refresher = services.auth.refresher;
        let instanceInput: Extract<RulesInput, { kind: 'instance' }> | undefined;
        const api: RulesApi = {
          propose: (input) => proposeRules(endpoint, refresher, input),
          test: (batchId, choice) => testRules(endpoint, refresher, batchId, choice),
          get: (batchId) => getRuleBatch(endpoint, refresher, batchId),
          approve: (decision) => approveRules(endpoint, refresher, decision),
        };
        const ui: RulesUi = {
          askInput: async () => {
            const prepared_ =
              typeof prepared?.sourceId === 'string' && prepared.sourceId
                ? prepared.sourceId
                : undefined;
            if (prepared_) return { kind: 'training_source', sourceId: prepared_ };
            const kind = await vscode.window.showQuickPick(
              [
                {
                  label: 'Registered training source',
                  description: 'replenishment data already registered with Evolve',
                  instance: false,
                },
                {
                  label: 'CSV-start instance (P0/P5/P7)',
                  description: 'rules for one exact instance file, tested by replay',
                  instance: true,
                },
              ],
              { placeHolder: 'What should the rules be discovered for?', ignoreFocusOut: true },
            );
            if (!kind) return undefined;
            if (kind.instance) {
              const instance = await readInstance();
              if (!instance) return undefined;
              instanceInput = { kind: 'instance', instance };
              return instanceInput;
            }
            const sourceId = await vscode.window.showInputBox({
              prompt: 'Registered Evolve training-source ID to discover rules in',
              ignoreFocusOut: true,
            });
            return sourceId ? { kind: 'training_source', sourceId } : undefined;
          },
          askPolicy: async () => {
            const chosen = await vscode.window.showQuickPick(
              [
                {
                  label: 'Use the service default',
                  description: 'recommended; the report states the policy that was applied',
                  custom: false,
                },
                {
                  label: 'Choose p_max and alpha\u2026',
                  description: `p_max up to ${percent(P_MAX_CAP)}, alpha up to ${percent(ALPHA_CAP)}`,
                  custom: true,
                },
              ],
              {
                placeHolder:
                  'Test policy. The test runs once per batch: a wrong choice cannot be redone.',
                ignoreFocusOut: true,
              },
            );
            if (!chosen) return undefined;
            if (!chosen.custom) return {};
            const ask = async (which: 'p_max' | 'alpha', cap: number) => {
              const text = await vscode.window.showInputBox({
                title: `Test policy: ${which}`,
                prompt:
                  which === 'p_max'
                    ? `p_max in percent: the highest breach rate you would tolerate. At most ${percent(cap)}. Empty keeps the service default. Not redoable once the test has run.`
                    : `alpha in percent: the chance of wrongly supporting a rule you accept. At most ${percent(cap)}. Empty keeps the service default. Not redoable once the test has run.`,
                placeHolder: 'for example 5',
                ignoreFocusOut: true,
                validateInput: (value) => {
                  if (value.trim() === '') return undefined;
                  const read = parsePolicyInput(value, which);
                  return read.ok ? undefined : read.message;
                },
              });
              if (text === undefined) return { cancelled: true as const };
              if (text.trim() === '') return { cancelled: false as const, value: undefined };
              const read = parsePolicyInput(text, which);
              if (!read.ok) return { cancelled: true as const };
              return { cancelled: false as const, value: read.value };
            };
            const pMax = await ask('p_max', P_MAX_CAP);
            if (pMax.cancelled) return undefined;
            const alpha = await ask('alpha', ALPHA_CAP);
            if (alpha.cancelled) return undefined;
            const choice: PolicyChoice = {};
            if (pMax.value !== undefined) choice.pMax = pMax.value;
            if (alpha.value !== undefined) choice.alpha = alpha.value;
            return choice;
          },
          confirmTest: async (proposal, choice) =>
            (await vscode.window.showWarningMessage(
              `Run the one confirmation test on ${proposal.candidates.length} candidate rule${proposal.candidates.length === 1 ? '' : 's'}? It cannot be redone.`,
              { modal: true, detail: confirmTestDetail(proposal, choice, instanceInput?.instance) },
              'Run confirmation test',
            )) === 'Run confirmation test',
          showReport: async (markdown) => {
            const path = `/rules-${createHash('sha256').update(markdown).digest('hex').slice(0, 12)}.txt`;
            reports.set(path, markdown);
            const document = await vscode.workspace.openTextDocument(
              vscode.Uri.from({ scheme: REPORT_SCHEME, path }),
            );
            await vscode.window.showTextDocument(document, { preview: false });
          },
          pickRules: async (supported) => {
            const picked = await vscode.window.showQuickPick(ruleChoices(supported), {
              canPickMany: true,
              ignoreFocusOut: true,
              placeHolder: 'Choose the supported rules to approve (none are pre-selected)',
            });
            return picked?.map((item) => item.hash);
          },
          confirmApproval: async (summary) =>
            (await vscode.window.showWarningMessage(
              'Approve these rules?',
              { modal: true, detail: summary },
              'Approve rules',
            )) === 'Approve rules',
          notify: (message) => void vscode.window.showInformationMessage(message),
        };
        const outcome = await reviewRules({ ui, api, subject: () => services.subject() });
        if (outcome.state === 'nothing_to_approve') {
          void vscode.window.showInformationMessage(
            'No rule was supported by the test, so nothing can be approved. Insufficient evidence means too few confirmation rows, not that the rule is wrong.',
          );
          return;
        }
        if (outcome.state !== 'approved') return;
        const { approval } = outcome;
        const action = await vscode.window.showInformationMessage(
          `Approved ${approval.rules.length} rule${approval.rules.length === 1 ? '' : 's'}. Approved rule set SHA-256: ${approval.ruleSetSha256}. It applies to new ${outcome.input.kind === 'instance' ? 'experiments on exactly this instance' : 'replenishment experiments'} started with this hash.${approval.policy ? ` Test policy: ${describePolicy(approval.policy)}.` : ''}`,
          START_LABEL,
        );
        if (action !== START_LABEL) return;
        const owner = await services.subject();
        const started = await startWithRules({
          approval,
          input: outcome.input,
          readSpec:
            outcome.input.kind === 'instance'
              ? async () => (outcome.input.kind === 'instance' ? outcome.input.instance : undefined)
              : readStartSpec,
          confirmStart: async (summary) =>
            (await vscode.window.showWarningMessage(
              'Start this experiment with the approved rules?',
              { modal: true, detail: summary },
              'Start experiment',
            )) === 'Start experiment',
          start: async (spec, hash) => {
            if ((await services.subject()) !== owner)
              throw new Error('The signed-in owner changed during review. Sign in again.');
            return startWithApprovedRules(endpoint, refresher, spec, hash);
          },
        });
        if (started)
          void vscode.window.showInformationMessage(
            `Experiment ${started.experiment} accepted by Evolve (${started.state}) with approved rule set ${approval.ruleSetSha256}.`,
          );
      } catch (error) {
        void vscode.window.showErrorMessage(`Evolve rule review failed: ${describeError(error)}`);
      }
    },
  );
  context.subscriptions.push(command);
  return command;
}
