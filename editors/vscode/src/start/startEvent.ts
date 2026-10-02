import type { CaptureEventRequest } from '../client';
import type { StartMode } from '../shared/protocol';
import { pageSlug } from '../shared/pageId';

/** The gateway's two modes for a manual start (`provenance.manual.mode`). */
export type ManualMode = 'run' | 'plan';

/**
 * The split button's mode as the gateway names it. The terminal is not a captured event at
 * all (it mints a token and opens a shell), so it has no mode here.
 */
export function manualModeFor(mode: StartMode): ManualMode | undefined {
  if (mode === 'background') return 'run';
  if (mode === 'plan') return 'plan';
  return undefined;
}

export interface StartRequest {
  skill: string;
  pageId: string;
  mode: ManualMode;
  /** Only sent when non-blank. The runner honours it within its allow-list. */
  harness?: string | undefined;
}

function manual(mode: ManualMode, harness: string | undefined, extra: Record<string, string> = {}) {
  const name = harness?.trim();
  return { manual: { mode, ...(name ? { harness: name } : {}), ...extra } };
}

function capture(skill: string, pageId: string, provenance: Record<string, unknown>) {
  const event: CaptureEventRequest = {
    label_skill: skill,
    instance_page_id: pageId,
    source: 'workbench',
    mime: 'text/plain',
    title: `${skill} · ${pageSlug(pageId)}`,
    body: 'Started from the workbench.',
    provenance,
  };
  return event;
}

/**
 * Start a skill on an instance (SPEC §3.9): a user event for the skill, aimed at the instance,
 * marked as a manual start. `requested_by` is deliberately absent: the gateway writes it from
 * the token, and a caller-supplied value is replaced, never trusted.
 */
export function buildStartEvent(req: StartRequest): CaptureEventRequest {
  return capture(req.skill, req.pageId, manual(req.mode, req.harness));
}

/**
 * Approve a plan: run the skill again, carrying the plan-mode run whose plan the runner
 * should execute (`provenance.manual.approved_plan_run_id`).
 */
export function buildApprovalEvent(req: {
  skill: string;
  pageId: string;
  planRunId: string;
  harness?: string | undefined;
}): CaptureEventRequest {
  const plan = req.planRunId.trim();
  if (!plan) throw new Error('approving a plan needs the plan run it approves');
  return capture(req.skill, req.pageId, manual('run', req.harness, { approved_plan_run_id: plan }));
}
