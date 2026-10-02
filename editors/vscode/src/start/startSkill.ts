import * as vscode from 'vscode';
import { EscurelError } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { pageSlug } from '../shared/pageId';
import type { StartMode } from '../shared/protocol';
import type { Services } from '../services';
import { pickTarget } from './pickTarget';
import { watchPlan } from './planWatch';
import { buildStartEvent } from './startEvent';

export interface StartSkillParams {
  skill?: string;
  pageId?: string;
  mode?: StartMode;
}

export type StartSkillInput =
  | { skill: string; pageId: string; mode: StartMode }
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

  return {
    skill: skill ?? (pageId ? skillFromPageId(pageId) : undefined),
    pageId,
    mode,
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
      if (!mode) {
        const modeItems: ModeQuickPickItem[] = [
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
        } catch {
          void vscode.window.showInformationMessage('Could not start in a terminal.');
        }
        return;
      }

      // 5. Capture event and open thread
      try {
        const event = await client.captureEvent(action.event);
        await vscode.commands.executeCommand('escurel.openThread', event.event_id);

        if (action.mode === 'background') {
          void vscode.window.showInformationMessage(startedMessage(skill, pageId));
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
              const choice = await vscode.window.showInformationMessage(
                planReadyMessage(startedSkill, startedPage),
                'Approve plan',
              );
              if (choice === 'Approve plan') {
                await vscode.commands.executeCommand('escurel.approvePlan', {
                  runId: res.runId,
                  skill,
                  pageId,
                });
              }
            } else if (res.state === 'failed') {
              void vscode.window.showErrorMessage(`Plan failed: ${res.reason}`);
            } else if (res.state === 'timeout') {
              void vscode.window.showWarningMessage(`Timed out waiting for plan for ${skill}`);
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
