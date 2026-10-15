import * as vscode from 'vscode';
import { createHash } from 'node:crypto';
import type { Services } from '../services';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { evolveOrigin, requireReviewedFileBytes } from './holdoutClient';
import {
  approveRules,
  getRuleBatch,
  proposeRules,
  startWithApprovedRules,
  testRules,
} from './rulesClient';
import { reviewRules, startWithRules, type RulesApi, type RulesUi } from './rulesReview';

const START_FILE_LIMIT = 4_000_000;

/** The person's own start file: an open (saved) JSON editor, or one they choose. */
async function readStartSpec(): Promise<Record<string, unknown> | undefined> {
  const open = vscode.workspace.textDocuments
    .filter((doc) => doc.uri.scheme === 'file' && doc.uri.path.endsWith('.json'))
    .slice(0, 12);
  const choices = [
    ...open.map((doc) => ({
      label: `Use open file ${doc.uri.path.split('/').pop() ?? ''}`,
      description: doc.uri.fsPath,
      doc,
    })),
    {
      label: 'Choose local JSON file',
      description: 'The new experiment\u2019s start request',
      doc: undefined,
    },
  ];
  const selected = await vscode.window.showQuickPick(choices, {
    placeHolder: 'Choose the new replenishment experiment start file',
  });
  if (!selected) return undefined;
  const doc = selected.doc;
  const file = doc
    ? doc.uri
    : (
        await vscode.window.showOpenDialog({
          canSelectMany: false,
          openLabel: 'Choose start file',
          filters: { JSON: ['json'] },
        })
      )?.[0];
  if (!file) return undefined;
  const bytes = await vscode.workspace.fs.readFile(file);
  if (doc) requireReviewedFileBytes(bytes, doc.getText(), doc.isDirty);
  if (bytes.length > START_FILE_LIMIT) throw new Error('The start file exceeds the 4 MB limit.');
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new Error('The start file is not valid JSON.');
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed))
    throw new Error('The start file must be one JSON object.');
  return parsed as Record<string, unknown>;
}

/** The report is shown read-only: a virtual document, so it can neither be edited nor left "unsaved". */
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
        const api: RulesApi = {
          propose: (sourceId) => proposeRules(endpoint, refresher, sourceId),
          test: (batchId) => testRules(endpoint, refresher, batchId),
          get: (batchId) => getRuleBatch(endpoint, refresher, batchId),
          approve: (decision) => approveRules(endpoint, refresher, decision),
        };
        const ui: RulesUi = {
          askSourceId: async () =>
            typeof prepared?.sourceId === 'string' && prepared.sourceId
              ? prepared.sourceId
              : vscode.window.showInputBox({
                  prompt: 'Registered Evolve training-source ID to discover rules in',
                  ignoreFocusOut: true,
                }),
          confirmTest: async (proposal) =>
            (await vscode.window.showWarningMessage(
              `Run the one confirmation test on ${proposal.candidates.length} candidate rule${proposal.candidates.length === 1 ? '' : 's'}?`,
              {
                modal: true,
                detail: [
                  `Batch ${proposal.batchId}`,
                  `Found in the first ${proposal.dayCut} of ${proposal.days} training days; they are tested on the rest.`,
                  '',
                  ...proposal.candidates.slice(0, 25).map((c) => `- ${c.text}`),
                  ...(proposal.candidates.length > 25
                    ? [`… and ${proposal.candidates.length - 25} more`]
                    : []),
                  '',
                  'A batch is tested once: asking again returns the stored report, and every candidate counts toward the multiple-testing correction for this data.',
                ].join('\n'),
              },
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
            const picked = await vscode.window.showQuickPick(
              supported.map((e) => ({
                label: e.text,
                description: `${e.confirmation.rows} rows, ${e.confirmation.breaches} breaches, p ${e.pValue}`,
                detail: `rule ${e.ruleSha256}`,
                hash: e.ruleSha256,
              })),
              {
                canPickMany: true,
                ignoreFocusOut: true,
                placeHolder: 'Choose the supported rules to approve (none are pre-selected)',
              },
            );
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
          `Approved ${approval.rules.length} rule${approval.rules.length === 1 ? '' : 's'}. Approved rule set SHA-256: ${approval.ruleSetSha256}. It applies to new replenishment experiments started with this hash.`,
          'Start new experiment with these rules',
        );
        if (action !== 'Start new experiment with these rules') return;
        const owner = await services.subject();
        const started = await startWithRules({
          approval,
          sourceId: outcome.sourceId,
          readSpec: readStartSpec,
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
