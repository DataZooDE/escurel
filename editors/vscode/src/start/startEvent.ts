import type { CaptureEventRequest } from '../client';
import { randomUUID } from 'node:crypto';
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
  // A blank page is "no target instance", a real choice: the key is left out (an empty string is
  // not "none"), and the event is named for the skill alone.
  const target = pageId.trim();
  const event: CaptureEventRequest = {
    label_skill: skill,
    ...(target ? { instance_page_id: target } : {}),
    source: 'workbench',
    mime: 'text/plain',
    // The Inbox row is "<label_skill> · <title>", so the skill is not repeated here.
    title: target ? pageSlug(target) : 'Started from the workbench',
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

/** Bind a Workbench validation click to the reviewed page and winner. */
export function bindValidationSelection(
  event: CaptureEventRequest,
  pageSha256: string,
  winnerProgramId: number,
): CaptureEventRequest {
  if (event.label_skill !== 'evolve_validate' || !/^[a-f0-9]{64}$/i.test(pageSha256)
      || !Number.isSafeInteger(winnerProgramId) || winnerProgramId < 0) {
    throw new Error('Validation needs a reviewed experiment page and exact winner.');
  }
  const manual = (event.provenance as { manual: Record<string, unknown> }).manual;
  return {
    ...event,
    event_id: `evolve-validation-${pageSha256.toLowerCase()}-${winnerProgramId}`,
    provenance: { manual: {
      ...manual,
      expected_page_sha256: pageSha256,
      expected_winner_program_id: winnerProgramId,
    } },
  };
}

/** Bind an owner-confirmed candidate request to the displayed private report. */
export function bindCandidateSelection(
  event: CaptureEventRequest,
  pageSha256: string,
  winnerProgramId: number,
  reportSha256: string,
  note: string,
): CaptureEventRequest {
  if (event.label_skill !== 'evolve_publish_candidate'
      || !/^[a-f0-9]{64}$/i.test(pageSha256)
      || !/^[a-f0-9]{64}$/i.test(reportSha256)
      || !Number.isSafeInteger(winnerProgramId) || winnerProgramId < 0
      || note.length > 1000) {
    throw new Error('Candidate publication needs a reviewed passed report and exact winner.');
  }
  const manual = (event.provenance as { manual: Record<string, unknown> }).manual;
  return {
    ...event,
    // A fresh human confirmation gets a new thread. The captured event object
    // keeps this ID stable for transport retry; the domain intent stays
    // single-writer for the experiment.
    event_id: `evolve-candidate-${randomUUID()}`,
    provenance: { manual: {
      ...manual,
      expected_page_sha256: pageSha256,
      expected_winner_program_id: winnerProgramId,
      expected_validation_report_sha256: reportSha256,
      confirm: true,
      review_note: note,
    } },
  };
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
  const event = capture(req.skill, req.pageId, manual('run', req.harness, { approved_plan_run_id: plan }));
  // A lost capture response followed by another click or editor restart must
  // reattach to the same approval. Escurel scopes event IDs to the tenant.
  if (req.skill === 'evolve_run') event.event_id = `evolve-approval-${plan}`;
  return event;
}
