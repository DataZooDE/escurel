import { notify } from '../commands/notify';
import { quietly } from '../shared/quiet';
import * as vscode from 'vscode';
import { EscurelError } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { readPageMarkdown } from '../fs/read';
import { pageSlug } from '../shared/pageId';
import type { StartMode } from '../shared/protocol';
import type { Services } from '../services';
import { pickTarget } from './pickTarget';
import { watchPlan } from './planWatch';
import {
  bindCandidateSelection,
  bindComparisonSelection,
  bindValidationSelection,
  buildStartEvent,
} from './startEvent';
import { isEvolveReviewControl } from '../shared/evolveControls';
import { preflightRequest, waitForPreflight } from '../evolve/preflight';

export interface StartSkillParams {
  skill?: string;
  pageId?: string;
  mode?: StartMode;
  expectedPageSha256?: string;
}

export type StartSkillInput =
  | { skill: string; pageId: string; mode: StartMode; expectedPageSha256?: string }
  | { kind: 'skill'; skill: { id: string } }
  | { kind: 'instance'; pageId: string; skill?: string }
  | StartSkillParams
  | undefined;

/**
 * Derives a skill name from an instance page id.
 * Handles flat `markdown/instances/<skill>__<id>.md` and nested `markdown/instances/<skill>/<id>.md`.
 */
export function skillFromPageId(pageId: string): string | undefined {
  const clean = pageId.replace(/^markdown\//, '');
  const parts = clean.split('/').filter(Boolean);
  if (parts[0] === 'instances') {
    if (parts.length >= 3) {
      return parts[1];
    }
    const file = parts.at(-1)?.replace(/\.md$/, '') ?? '';
    const sep = file.indexOf('__');
    if (sep > 0) {
      return file.slice(0, sep);
    }
  }
  return undefined;
}

/**
 * Parses any supported startSkill input shape into partial { skill, pageId, mode }.
 */
export function parseStartSkillInput(arg: unknown): StartSkillParams {
  if (!arg || typeof arg !== 'object') {
    return { skill: undefined, pageId: undefined, mode: undefined };
  }

  const obj = arg as Record<string, unknown>;

  // Shape 1: tree node { kind: 'skill', skill: { id: string } }
  if (obj.kind === 'skill' && typeof obj.skill === 'object' && obj.skill !== null) {
    const s = obj.skill as { id?: unknown };
    return {
      skill: typeof s.id === 'string' ? s.id : undefined,
      pageId: undefined,
      mode: undefined,
    };
  }

  // Shape 2: tree node { kind: 'instance', pageId: string, skill?: string }
  if (obj.kind === 'instance' && typeof obj.pageId === 'string') {
    const skill =
      typeof obj.skill === 'string' && obj.skill ? obj.skill : skillFromPageId(obj.pageId);
    return {
      skill,
      pageId: obj.pageId,
      mode: undefined,
    };
  }

  // Shape 3: page-as-UI or arbitrary params
  const skill = typeof obj.skill === 'string' ? obj.skill : undefined;
  const pageId = typeof obj.pageId === 'string' ? obj.pageId : undefined;
  const mode =
    typeof obj.mode === 'string' && ['background', 'plan', 'terminal'].includes(obj.mode)
      ? (obj.mode as StartMode)
      : undefined;
  const expectedPageSha256 = typeof obj.expectedPageSha256 === 'string'
    ? obj.expectedPageSha256 : undefined;

  return {
    skill: skill ?? (pageId ? skillFromPageId(pageId) : undefined),
    pageId,
    mode,
    expectedPageSha256,
  };
}

export type StartAction =
  | {
      type: 'capture';
      mode: 'background' | 'plan';
      event: ReturnType<typeof buildStartEvent>;
    }
  | {
      type: 'terminal';
      command: 'escurel.startInTerminal';
      args: { skill: string; pageId: string };
    };

export function resolveStartAction(req: {
  skill: string;
  pageId: string;
  mode: StartMode;
  harness?: string;
}): StartAction {
  if (req.mode === 'terminal') {
    return {
      type: 'terminal',
      command: 'escurel.startInTerminal',
      args: { skill: req.skill, pageId: req.pageId },
    };
  }

  if (req.mode === 'plan') {
    return {
      type: 'capture',
      mode: 'plan',
      event: buildStartEvent({
        skill: req.skill,
        pageId: req.pageId,
        mode: 'plan',
        harness: req.harness,
      }),
    };
  }

  return {
    type: 'capture',
    mode: 'background',
    event: buildStartEvent({
      skill: req.skill,
      pageId: req.pageId,
      mode: 'run',
      harness: req.harness,
    }),
  };
}

export function formatStartError(err: unknown): string {
  if (err instanceof EscurelError && err.kind === 'forbidden') {
    return 'You are not allowed to start this skill here.';
  }
  const desc = describeError(err);
  return desc.split('\n')[0] ?? desc;
}

interface SkillQuickPickItem extends vscode.QuickPickItem {
  skillId: string;
}

interface ModeQuickPickItem extends vscode.QuickPickItem {
  mode: StartMode;
}

/**
 * Registers `escurel.startSkill`.
 */
export function registerStartSkill(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const disposable = vscode.commands.registerCommand(
    'escurel.startSkill',
    async (arg?: unknown) => {
      const parsed = parseStartSkillInput(arg);
      let skill = parsed.skill;
      let pageId = parsed.pageId;
      let mode = parsed.mode;
      const reviewedPageSha256 = parsed.expectedPageSha256;

      const client = services.client;

      // 1. Ask for skill if missing
      if (!skill) {
        try {
          const skills = await client.listSkills();
          const items: SkillQuickPickItem[] = skills.map((s) => ({
            label: s.id,
            description: s.summary ?? s.description ?? '',
            skillId: s.id,
          }));
          const picked = await vscode.window.showQuickPick(items, {
            placeHolder: 'Select a skill to start',
            matchOnDescription: true,
          });
          if (!picked) {
            return; // cancelled
          }
          skill = picked.skillId;
        } catch (err) {
          void vscode.window.showErrorMessage(formatStartError(err));
          return;
        }
      }

      // 2. Ask for target instance if missing
      if (pageId === undefined) {
        try {
          const pickedTarget = await pickTarget(client, skill);
          if (pickedTarget === undefined) {
            return; // cancelled
          }
          pageId = pickedTarget;
        } catch (err) {
          void vscode.window.showErrorMessage(formatStartError(err));
          return;
        }
      }

      // 3. Ask for mode if missing
      if (skill === 'evolve_run' && mode && mode !== 'plan') {
        void vscode.window.showErrorMessage(
          'Evolve requires a reviewed plan. Choose “Make a plan first”, then approve the completed plan.',
        );
        return;
      }
      if (isEvolveReviewControl(skill) && mode && mode !== 'background') {
        void vscode.window.showErrorMessage('This Evolve review action runs in the background after the evidence is reviewed.');
        return;
      }
      if (!mode) {
        const modeItems: ModeQuickPickItem[] = skill === 'evolve_publish_candidate'
          ? [{ label: 'Create policy candidate', description: 'Confirm publication of an inactive candidate', mode: 'background' }]
          : skill === 'evolve_compare'
          ? [{ label: 'Compute comparison', description: 'Replay both programs on the training instance (no model spend)', mode: 'background' }]
          : skill === 'evolve_validate'
          ? [{ label: 'Validate winner', description: 'Run one operator-attested finite-horizon replay', mode: 'background' }]
          : skill === 'evolve_run'
          ? [{
              label: 'Make a plan first',
              description: 'Review the exact problem revision before approving a search',
              mode: 'plan',
            }]
          : [
          {
            label: 'Start in background',
            description: 'Run the skill with an agent',
            mode: 'background',
          },
          {
            label: 'Make a plan first',
            description: 'Propose steps before running',
            mode: 'plan',
          },
          {
            label: 'Start in terminal',
            description: 'Run interactively in a terminal',
            mode: 'terminal',
          },
        ];
        const pickedMode = await vscode.window.showQuickPick(modeItems, {
          placeHolder: `Choose how to run ${skill}`,
        });
        if (!pickedMode) {
          return; // cancelled
        }
        mode = pickedMode.mode;
      }

      // 4. Resolve action
      const config = readConfig();
      const action = resolveStartAction({
        skill,
        pageId,
        mode,
        harness: config.harness,
      });

      if (action.type === 'terminal') {
        try {
          await vscode.commands.executeCommand(action.command, action.args);
        } catch (error) {
          // Say why: the person's next step depends on it (not signed in, no permission, a bad setting).
          void vscode.window.showWarningMessage(
            `Could not start in a terminal: ${describeError(error)}`,
          );
        }
        return;
      }

      // 5. Capture event and open thread
      try {
        if (action.event.label_skill === 'evolve_run') {
          const page = await readPageMarkdown(client, pageId);
          if (page?.skill !== 'evolve_problem' || !page.sha256 || page.degraded) {
            throw new Error('The Evolve problem page is unavailable or has no verified revision.');
          }
          if (!reviewedPageSha256) {
            throw new Error('Open the Evolve problem page and review its current revision before starting.');
          }
          if (page.sha256 !== reviewedPageSha256) {
            throw new Error('The Evolve problem changed since it was displayed. Refresh the page and review it again.');
          }
          const provenance = action.event.provenance as Record<string, unknown>;
          const manual = provenance.manual as Record<string, unknown>;
          manual.expected_page_sha256 = reviewedPageSha256;
          const check = await client.captureEvent(preflightRequest(pageId, reviewedPageSha256));
          const result = await vscode.window.withProgress({
            location: vscode.ProgressLocation.Notification,
            title: 'Checking Evolve problem and private holdout binding',
            cancellable: true,
          }, async (_progress, token) => waitForPreflight({
            rootEventId: check.event_id,
            pageSha256: reviewedPageSha256,
            isCancelled: () => token.isCancellationRequested,
            listEvents: () => client.listEvents({
              root_event_id: check.event_id,
              label_skill: 'evolve:preflight',
              include_system: true,
            }),
          }));
          if (!result.ready) {
            await vscode.commands.executeCommand('escurel.openThread', check.event_id);
            throw new Error(`Evolve problem preflight found issues: ${result.issue} Re-import a corrected training spec, then review the new page revision.`);
          }
          if (result.holdoutContract) {
            const reviewed = await vscode.window.showInformationMessage(
              `Review the frozen private holdout contract before planning:\n${result.holdoutContract}\n\n`
              + 'Structural and binding checks passed. Baseline, seed, and provider readiness are checked during durable search.',
              { modal: true }, 'Review experiment plan',
            );
            if (reviewed !== 'Review experiment plan') return;
          } else {
            void vscode.window.showInformationMessage(
              'Structural and holdout-binding checks passed. Baseline, seed, and provider readiness are checked during the durable search; this is ready for plan review.',
            );
          }
        }
        if (action.event.label_skill === 'evolve_validate') {
          const page = await readPageMarkdown(client, pageId);
          if (page?.skill !== 'evolve_experiment' || !page.sha256 || page.degraded
              || !reviewedPageSha256 || page.sha256 !== reviewedPageSha256) {
            throw new Error('The experiment changed since it was displayed. Refresh and review its winner.');
          }
          const winner = page.frontmatter.best_program_id;
          if (page.frontmatter.next_validation_action !== 'evolve_validate_winner'
              || !Number.isSafeInteger(winner) || typeof winner !== 'number') {
            throw new Error('This experiment has no winner ready for the declared holdout replay.');
          }
          action.event = bindValidationSelection(action.event, reviewedPageSha256, winner);
        }
        if (action.event.label_skill === 'evolve_compare') {
          const page = await readPageMarkdown(client, pageId);
          if (page?.skill !== 'evolve_comparison' || !page.sha256 || page.degraded
              || !reviewedPageSha256 || page.sha256 !== reviewedPageSha256) {
            throw new Error('The comparison page changed since it was displayed. Refresh and review it again.');
          }
          if (page.frontmatter.status !== 'requested'
              || page.frontmatter.next_comparison_action !== 'evolve_compare'
              || typeof page.frontmatter.experiment !== 'string') {
            throw new Error('This comparison is not waiting to be computed.');
          }
          action.event = bindComparisonSelection(action.event, reviewedPageSha256);
        }
        if (action.event.label_skill === 'evolve_publish_candidate') {
          const page = await readPageMarkdown(client, pageId);
          if (page?.skill !== 'evolve_validation_report' || !page.sha256 || page.degraded
              || !reviewedPageSha256 || page.sha256 !== reviewedPageSha256) {
            throw new Error('The validation report changed since it was displayed. Refresh and review it again.');
          }
          const winner = page.frontmatter.winner_program_id;
          const reportHash = page.frontmatter.report_sha256;
          if (page.frontmatter.next_candidate_action !== 'evolve_publish_candidate'
              || page.frontmatter.effective_passed !== true || page.frontmatter.status !== 'passed'
              || !Number.isSafeInteger(winner) || typeof winner !== 'number'
              || typeof reportHash !== 'string' || !/^[a-f0-9]{64}$/i.test(reportHash)) {
            throw new Error('This report does not support candidate publication.');
          }
          const sandboxWarning = page.frontmatter.candidate_use === 'sandbox_demo_only'
            ? ' This disclosed synthetic candidate is for sandbox use only and must never be activated as an operational policy.'
            : '';
          const confirmed = await vscode.window.showWarningMessage(
            `Create an inactive policy candidate for winner ${winner} from this exact report? This does not activate a policy.${sandboxWarning}`,
            { modal: true },
            'Create candidate',
          );
          if (confirmed !== 'Create candidate') return;
          const note = await vscode.window.showInputBox({
            prompt: 'Optional reviewer note for the candidate page',
            validateInput: (value) => value.length > 1000 ? 'Keep the note under 1000 characters.' : undefined,
          });
          if (note === undefined) return;
          action.event = bindCandidateSelection(action.event, reviewedPageSha256, winner, reportHash, note);
        }
        const event = await client.captureEvent(action.event);
        await vscode.commands.executeCommand('escurel.openThread', event.event_id);

        if (action.mode === 'background') {
          quietly(startedMessage(skill, pageId));
          return;
        }

        // Mode is plan: watch for the plan DETACHED. The command is done once the event is
        // captured and its thread is open; awaiting the watcher (and the "Approve plan?" notice,
        // which waits for a click) would hold the command open for as long as five minutes, and
        // anything that awaits `executeCommand('escurel.startSkill')` with it.
        // `skill` and `pageId` are narrowed here; a closure would lose that, so take them as constants.
        const startedSkill = skill;
        const startedPage = pageId;
        void offerApproval();
        async function offerApproval(): Promise<void> {
          let cancelled = false;
          const cancelSub = {
            dispose() {
              cancelled = true;
            },
          };
          context.subscriptions.push(cancelSub);
          const configSub = services.onDidChange(() => {
            cancelled = true;
          });
          try {
            const res = await watchPlan({
              rootEventId: event.event_id,
              fetchLineage: (rootId) => client.listLineage({ root_event_id: rootId }),
              isCancelled: () => cancelled,
            });

            if (res.state === 'planned') {
              const approveChoice = startedSkill === 'evolve_run' ? 'Review search limits' : 'Approve plan';
              const choice = await vscode.window.showInformationMessage(
                planReadyMessage(startedSkill, startedPage),
                approveChoice,
                'Open thread',
              );
              if (choice === approveChoice) {
                await vscode.commands.executeCommand('escurel.approvePlan', {
                  runId: res.runId,
                  skill,
                  pageId,
                });
              } else if (choice === 'Open thread') {
                await vscode.commands.executeCommand('escurel.openThread', event.event_id);
              }
            } else if (res.state === 'failed') {
              void notify('error', `Plan failed: ${res.reason}`, [
                { kind: 'thread', rootEventId: event.event_id },
                { kind: 'run', runId: res.runId },
              ]);
            } else if (res.state === 'timeout') {
              void vscode.window
                .showWarningMessage(
                  `The plan for ${skill} is not ready yet. Open the thread to see where it is.`,
                  'Open thread',
                )
                .then((pick) => {
                  if (pick === 'Open thread') {
                    void vscode.commands.executeCommand('escurel.openThread', event.event_id);
                  }
                });
            }
          } catch (err) {
            void vscode.window.showErrorMessage(formatStartError(err));
          } finally {
            configSub.dispose();
            cancelSub.dispose();
          }
        }
      } catch (err) {
        void vscode.window.showErrorMessage(formatStartError(err));
      }
    },
  );

  context.subscriptions.push(disposable);
  return disposable;
}

/** "Started <skill> on <instance>": the instance by its own id, whatever skill is being started. */
export function startedMessage(skill: string, pageId: string | undefined): string {
  const slug = pageId ? pageSlug(pageId) : '';
  return slug ? `Started ${skill} on ${slug}` : `Started ${skill}`;
}

export function planReadyMessage(skill: string, pageId: string | undefined): string {
  const slug = pageId ? pageSlug(pageId) : '';
  return slug ? `Plan ready for ${skill} on ${slug}` : `Plan ready for ${skill}`;
}
