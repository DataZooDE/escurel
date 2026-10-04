import * as vscode from 'vscode';
import { EscurelError, type EscurelClient } from '../client';
import { readConfig } from '../config';
import { describeError } from '../errors';
import { readPageMarkdown } from '../fs/read';
import { loadRun } from '../runs/loadRun';
import type { Services } from '../services';
import { buildApprovalEvent } from './startEvent';

export interface ApprovePlanArgs {
  runId: string;
  skill?: string;
  pageId?: string;
}

/**
 * Resolves the skill name and target page id for a plan approval.
 *
 * If either is missing, it reads the run detail (loadRun) for `view.targetPageId`
 * and fetches the lineage to find the root event's `label_skill`.
 * A missing subject throws an error — it is never guessed.
 */
export async function resolveApprovalSubject(
  client: EscurelClient,
  runId: string,
  hints?: { skill?: string; pageId?: string },
): Promise<{ skill: string; pageId: string }> {
  let skill = hints?.skill;
  let pageId = hints?.pageId;

  // An explicit empty page means the plan was started with no target instance; that is an answer,
  // not a gap to fill from the run.
  if (skill && pageId !== undefined) {
    return { skill, pageId };
  }

  const { view, rootEventId } = await loadRun(client, runId);
  if (pageId === undefined && view.targetPageId) {
    pageId = view.targetPageId;
  }

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

  return { skill, pageId };
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
  const disposable = vscode.commands.registerCommand(
    'escurel.approvePlan',
    async (arg?: unknown) => {
      const req = (typeof arg === 'object' && arg !== null ? arg : {}) as Partial<ApprovePlanArgs>;
      if (!req.runId || typeof req.runId !== 'string') {
        void vscode.window.showErrorMessage('Cannot approve plan: missing run ID');
        return;
      }

      try {
        const client = services.client;
        const subject = await resolveApprovalSubject(client, req.runId, {
          skill: req.skill,
          pageId: req.pageId,
        });

        const config = readConfig();
        const eventReq = buildApprovalEvent({
          skill: subject.skill,
          pageId: subject.pageId,
          planRunId: req.runId,
          harness: config.harness,
        });
        if (subject.skill === 'evolve_run') {
          if (!eventReq.event_id) throw new Error('The Evolve approval has no retry key.');
          const priorEventId = await existingEvolveApproval(
            client, eventReq.event_id, subject.pageId, req.runId,
          );
          if (priorEventId) {
            await vscode.commands.executeCommand('escurel.openThread', priorEventId);
            return;
          }
          const { rootEventId } = await loadRun(client, req.runId);
          if (!rootEventId) throw new Error('The Evolve plan has no initiating event. Make a new plan.');
          const frozen = await evolveApprovalRevision(client, rootEventId, subject.pageId);
          const provenance = eventReq.provenance as Record<string, unknown>;
          (provenance.manual as Record<string, unknown>).expected_page_sha256 = frozen;
        }

        const captured = await client.captureEvent(eventReq);
        await vscode.commands.executeCommand('escurel.openThread', captured.event_id);
      } catch (err) {
        void vscode.window.showErrorMessage(formatApprovalError(err));
      }
    },
  );

  context.subscriptions.push(disposable);
  return disposable;
}
