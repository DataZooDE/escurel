import type { CaptureEventRequest, Event } from '../client';
import { buildStartEvent } from '../start/startEvent';

export function preflightRequest(pageId: string, pageSha256: string): CaptureEventRequest {
  if (!/^[a-f0-9]{64}$/i.test(pageSha256)) {
    throw new Error('Preflight needs the exact displayed problem revision.');
  }
  const event = buildStartEvent({ skill: 'evolve_preflight', pageId, mode: 'run' });
  const manual = (event.provenance as { manual: Record<string, unknown> }).manual;
  manual.expected_page_sha256 = pageSha256;
  return event;
}

export function parsePreflightReceipt(
  receipt: Event,
  rootEventId: string,
  pageSha256: string,
): { ready: boolean; issue?: string } | undefined {
  if (receipt.kind !== 'system' || receipt.source !== 'anofox-evolve'
      || receipt.label_skill !== 'evolve:preflight'
      || receipt.root_event_id !== rootEventId || !receipt.body) return undefined;
  let result: Record<string, unknown>;
  try {
    result = JSON.parse(receipt.body) as Record<string, unknown>;
  } catch {
    return undefined;
  }
  if (result.problem_sha256 !== pageSha256) return undefined;
  if (result.structural_ready_for_start === true) return { ready: true };
  const preflight = result.preflight as Record<string, unknown> | undefined;
  const issues = preflight?.issues;
  const messages = Array.isArray(issues)
    ? issues.map((issue) => String((issue as Record<string, unknown>)?.message ?? issue)) : [];
  if (result.holdout_binding_issue) messages.push(String(result.holdout_binding_issue));
  if (result.issue) messages.push(String(result.issue));
  return {
    ready: false,
    issue: messages.join('; ') || 'The training specification needs review.',
  };
}

export async function waitForPreflight(args: {
  rootEventId: string;
  pageSha256: string;
  listEvents: () => Promise<{ events: Event[] }>;
  isCancelled?: () => boolean;
  sleep?: (ms: number) => Promise<void>;
  timeoutMs?: number;
}): Promise<{ ready: boolean; issue?: string }> {
  const start = Date.now();
  const timeout = args.timeoutMs ?? 60_000;
  const sleep = args.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  while (Date.now() - start < timeout) {
    if (args.isCancelled?.()) throw new Error('Problem preflight was cancelled.');
    const page = await args.listEvents();
    for (const receipt of page.events) {
      const result = parsePreflightReceipt(receipt, args.rootEventId, args.pageSha256);
      if (result) return result;
    }
    await sleep(750);
  }
  throw new Error('Problem preflight is still pending. Open its thread to inspect the result, then retry plan review.');
}
