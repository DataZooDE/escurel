import * as vscode from 'vscode';
import { EscurelError, type EscurelClient } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { readPageMarkdown } from '../fs/read';
import { inFlight } from '../runs/controlWait';
import { loadRun, type LoadedRun } from '../runs/loadRun';
import { cleanText } from '../shared/untrustedText';
import type { Services } from '../services';
import { buildApprovalEvent } from './startEvent';
import { evolveApprovalSummary } from '../evolve/approvalSummary';

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
  confirm: (message: string, action?: string) => Promise<boolean>;
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
  const subject = await resolveApprovalSubject(client, runId, loaded);
  const eventReq = buildApprovalEvent({
    skill: subject.skill,
    pageId: subject.pageId,
    planRunId: runId,
    harness: deps.harness,
  });
  if (subject.skill === 'evolve_run') {
    if (!eventReq.event_id) throw new Error('The Evolve approval has no retry key.');
    const prior = await existingEvolveApproval(client, eventReq.event_id, subject.pageId, runId);
    if (prior) return { kind: 'approved', eventId: prior };
  }
  if (loaded.view.status !== 'planned') {
    return { kind: 'not-planned', status: loaded.view.status };
  }
  let ok: boolean;
  if (subject.skill === 'evolve_run') {
    const { rootEventId, view } = loaded;
    if (!rootEventId || view.plan.length === 0) {
      throw new Error('The Evolve plan has no reviewable steps. Make a new plan.');
    }
    if (!view.harness || view.harness === 'echo') {
      throw new Error('Echo or unlabelled plans cannot authorize Evolve search. Configure a planning harness and make a new plan.');
    }
    const frozen = await evolveApprovalRevision(client, rootEventId, subject.pageId);
    const page = await readPageMarkdown(client, subject.pageId);
    if (!page || page.sha256 !== frozen || page.degraded) {
      throw new Error('The Evolve problem changed during approval. Review it and make a new plan.');
    }
    ok = await deps.confirm(evolveApprovalSummary(frozen, page.frontmatter.search_request,
      { harness: view.harness, steps: view.plan }), 'Approve search');
    if (ok) {
      const provenance = eventReq.provenance as Record<string, unknown>;
      const manual = provenance.manual as Record<string, unknown>;
      manual.harness = view.harness;
      manual.expected_page_sha256 = frozen;
    }
  } else {
    const where = subject.pageId ? ` on ${cleanText(subject.pageId, 120)}` : '';
    ok = await deps.confirm(
      `Approve the plan and run ${cleanText(subject.skill, 80)}${where}? Run ${cleanText(runId, 40)}. This starts the skill for real.`,
    );
  }
  if (!ok) return { kind: 'declined' };
  const captured = await client.captureEvent(eventReq);
  return { kind: 'approved', eventId: captured.event_id };
}

/** Asks in a modal; a test replaces it through the extension API (a modal blocks a headless window). */
let confirmApproval = async (message: string, action = 'Approve and run'): Promise<boolean> =>
  (await vscode.window.showWarningMessage(message, { modal: true }, action)) === action;

/** Test seam: swap the confirmation; returns the previous one. */
export function setApprovalConfirm(
  fn: (message: string, action?: string) => Promise<boolean>,
): (message: string, action?: string) => Promise<boolean> {
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

/** The approval may spend only against the bytes captured when this plan began. */
export async function evolveApprovalRevision(
  client: EscurelClient,
  rootEventId: string,
  pageId: string,
): Promise<string> {
  if (!rootEventId) throw new Error('The Evolve plan has no initiating event. Make a new plan.');
  const root = (await client.listEvents({ event_id: rootEventId })).events[0];
  const manual = root?.provenance?.manual;
  const plan = typeof manual === 'object' && manual !== null
    ? (manual as Record<string, unknown>) : undefined;
  const frozen = plan?.target_page_sha256;
  if (root?.kind !== 'user' || root.label_skill !== 'evolve_run' ||
      root.instance_page_id !== pageId || plan?.mode !== 'plan' ||
      root.revision_binding_attested !== true ||
      typeof frozen !== 'string' || !/^[0-9a-f]{64}$/.test(frozen)) {
    throw new Error('The Evolve plan is not bound to this problem revision. Make a new plan.');
  }
  const page = await readPageMarkdown(client, pageId);
  if (page?.skill !== 'evolve_problem' || page.sha256 !== frozen || page.degraded) {
    throw new Error('The Evolve problem changed after planning. Review it and make a new plan.');
  }
  return frozen;
}

/** Recover a previously accepted approval before considering a new page revision. */
export async function existingEvolveApproval(
  client: EscurelClient,
  eventId: string,
  pageId: string,
  planRunId: string,
): Promise<string | undefined> {
  const prior = (await client.listEvents({ event_id: eventId })).events[0];
  const manual = prior?.provenance?.manual;
  if (prior?.kind === 'user' && prior.label_skill === 'evolve_run' &&
      prior.instance_page_id === pageId &&
      typeof manual === 'object' && manual !== null &&
      (manual as Record<string, unknown>).approved_plan_run_id === planRunId) {
    if (prior.revision_binding_attested !== true) {
      throw new Error('This approval predates gateway revision attestation. Make a new plan.');
    }
    return prior.event_id;
  }
  return undefined;
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
            confirm: (m, action) => confirmApproval(m, action),
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
