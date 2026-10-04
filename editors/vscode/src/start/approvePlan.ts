import * as vscode from 'vscode';
import { EscurelError, type EscurelClient } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { inFlight } from '../runs/controlWait';
import { loadRun, type LoadedRun } from '../runs/loadRun';
import { cleanText } from '../shared/untrustedText';
import type { Services } from '../services';
import { buildApprovalEvent } from './startEvent';

export interface ApprovePlanArgs {
  runId: string;
}

/**
 * Resolves the skill name and target page id for a plan approval, from the RUN itself.
 *
 * Whoever invokes the command (a tree row, a webview button, another extension) names only the run:
 * a caller-supplied skill or page is never trusted, because the approval is sent as the user and
 * starts that skill on that page for real. The run detail (loadRun) gives `view.targetPageId`, the
 * lineage gives the root event's `label_skill`. A missing subject throws — it is never guessed.
 */
export async function resolveApprovalSubject(
  client: EscurelClient,
  runId: string,
  loaded?: LoadedRun,
): Promise<{ skill: string; pageId: string; status: string }> {
  const { view, rootEventId } = loaded ?? (await loadRun(client, runId));
  const pageId = view.targetPageId;
  let skill = view.skill;

  if (!skill && rootEventId) {
    try {
      const lineage = await client.listLineage({ root_event_id: rootEventId });
      const rootNode = lineage.nodes.find(
        (n) => n.type === 'event' && (n.id === rootEventId || n.parent === null),
      );
      if (typeof rootNode?.label_skill === 'string') {
        skill = rootNode.label_skill;
      }
    } catch {
      // Lineage read failed; handled by check below
    }
  }

  if (pageId === undefined && !skill) {
    throw new Error('Cannot approve plan: missing target page and skill');
  }
  if (pageId === undefined) {
    throw new Error('Cannot approve plan: missing target page');
  }
  if (!skill) {
    throw new Error('Cannot approve plan: missing skill');
  }

  return { skill, pageId, status: view.status };
}

/** What the approval ended in, for the caller to word. */
export type ApprovalOutcome =
  | { kind: 'approved'; eventId: string }
  | { kind: 'not-planned'; status: string }
  | { kind: 'declined' };

export interface ApprovalDeps {
  confirm: (message: string) => Promise<boolean>;
  harness: string;
}

/**
 * Approve a plan run: re-read the run, refuse unless it is STILL `planned`, ask the person to confirm
 * what is about to run, and only then capture the approval. The run is the source of truth, not the row
 * or button that was clicked (a stale Awaiting row must not approve a run that already moved on).
 */
export async function approvePlanRun(
  client: EscurelClient,
  runId: string,
  deps: ApprovalDeps,
): Promise<ApprovalOutcome> {
  const loaded = await loadRun(client, runId);
  if (loaded.view.status !== 'planned') {
    return { kind: 'not-planned', status: loaded.view.status };
  }
  const subject = await resolveApprovalSubject(client, runId, loaded);
  const where = subject.pageId ? ` on ${cleanText(subject.pageId, 120)}` : '';
  const ok = await deps.confirm(
    `Approve the plan and run ${cleanText(subject.skill, 80)}${where}? Run ${cleanText(runId, 40)}. This starts the skill for real.`,
  );
  if (!ok) return { kind: 'declined' };
  const captured = await client.captureEvent(
    buildApprovalEvent({
      skill: subject.skill,
      pageId: subject.pageId,
      planRunId: runId,
      harness: deps.harness,
    }),
  );
  return { kind: 'approved', eventId: captured.event_id };
}

/** Asks in a modal; a test replaces it through the extension API (a modal blocks a headless window). */
let confirmApproval = async (message: string): Promise<boolean> =>
  (await vscode.window.showWarningMessage(message, { modal: true }, 'Approve and run')) ===
  'Approve and run';

/** Test seam: swap the confirmation; returns the previous one. */
export function setApprovalConfirm(
  fn: (message: string) => Promise<boolean>,
): (message: string) => Promise<boolean> {
  const prev = confirmApproval;
  confirmApproval = fn;
  return prev;
}

function formatApprovalError(err: unknown): string {
  if (err instanceof EscurelError && err.kind === 'forbidden') {
    return 'You are not allowed to start this skill here.';
  }
  return describeError(err);
}

/**
 * Registers the `escurel.approvePlan` command.
 */
export function registerApprovePlan(
  context: vscode.ExtensionContext,
  services: Services,
): vscode.Disposable {
  const once = inFlight();
  const disposable = vscode.commands.registerCommand(
    'escurel.approvePlan',
    async (arg?: unknown) => {
      const req = (typeof arg === 'object' && arg !== null ? arg : {}) as Partial<ApprovePlanArgs>;
      if (!req.runId || typeof req.runId !== 'string') {
        void vscode.window.showInformationMessage(
          'Open the run that is waiting for approval (in its thread or in the Runner view), then approve its plan there.',
        );
        return;
      }

      try {
        // One approval per plan run at a time: a double click must not ask twice or approve twice.
        await once(req.runId, async () => {
          const outcome = await approvePlanRun(services.client, req.runId!, {
            confirm: (m) => confirmApproval(m),
            harness: readConfig().harness,
          });
          if (outcome.kind === 'not-planned') {
            void vscode.window.showInformationMessage(
              `This run is no longer waiting for approval (it is ${cleanText(outcome.status, 40)}).`,
            );
          } else if (outcome.kind === 'approved') {
            await vscode.commands.executeCommand('escurel.openThread', outcome.eventId);
          }
        });
      } catch (err) {
        void vscode.window.showErrorMessage(formatApprovalError(err));
      }
    },
  );

  context.subscriptions.push(disposable);
  return disposable;
}
