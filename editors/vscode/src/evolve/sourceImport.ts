import { createHash } from 'node:crypto';
import { stringify } from 'yaml';
import type { CaptureEventRequest, Event } from '../client';
import { buildStartEvent } from '../start/startEvent';

const SOURCE_FIELDS = [
  'capacity', 'skus', 'training_start', 'training_end',
  'history_start', 'history_end', 'inventory_as_of', 'demand_observation',
] as const;
const SOURCE_ONLY_FIELDS = new Set<string>(SOURCE_FIELDS);
const FULL_SPEC_FIELDS = new Set<string>([
  ...SOURCE_FIELDS, 'service_targets', 'seed_sql', 'baseline_sql',
  'planning_window_days', 'scored_window_days', 'unit_order_costs',
  'terminal_stock_tolerance', 'training_source_id', 'source_sha256',
  'max_generations', 'budget', 'num_islands', 'migration_interval',
  'operator', 'model_tier', 'model_ensemble', 'model_ensemble_strong_every',
  'capture_rationale',
]);

export function trainingSourcePayload(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Training source must be a JSON object.');
  const input = value as Record<string, unknown>;
  const fullSpec = ['service_targets', 'seed_sql', 'baseline_sql']
    .some((field) => field in input);
  for (const key of Object.keys(input)) {
    if (!(fullSpec ? FULL_SPEC_FIELDS : SOURCE_ONLY_FIELDS).has(key))
      throw new Error(`Unsupported source key ${key}. Choose an eight-field source or a full V2 training spec.`);
  }
  const source: Record<string, unknown> = {};
  for (const field of SOURCE_FIELDS) {
    if (!(field in input) || input[field] === null || input[field] === undefined)
      throw new Error(`Training source needs ${field}.`);
    source[field] = input[field];
  }
  if (!Number.isSafeInteger(source.capacity) || (source.capacity as number) <= 0)
    throw new Error('Capacity must be a positive integer.');
  if (!Array.isArray(source.skus) || source.skus.length === 0 || source.skus.length > 16)
    throw new Error('The V2 pilot needs 1–16 SKUs.');
  if (source.demand_observation !== 'true_demand' || source.inventory_as_of !== source.training_start)
    throw new Error('Attest true demand and opening inventory as of training_start.');
  for (const field of ['training_start', 'training_end', 'history_start', 'history_end'] as const) {
    if (typeof source[field] !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(source[field] as string))
      throw new Error(`${field} must be a YYYY-MM-DD date.`);
  }
  if (JSON.stringify(source).length > 500 * 1024)
    throw new Error('Workbench source pages support at most 500 KiB of JSON.');
  return source;
}

export function trainingSourcePage(args: {
  id: string; owner: string; value: unknown;
}): { pageId: string; content: string; payload: Record<string, unknown> } {
  const { id, owner, value } = args;
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(id) || id.length > 128)
    throw new Error('Use a lowercase source page ID of at most 128 letters, numbers, underscores, or hyphens.');
  if (!owner.trim()) throw new Error('Sign in before preparing an owner-private source.');
  const payload = trainingSourcePayload(value);
  const frontmatter = stringify({
    kind: 'instance', skill: 'evolve_training_source', id, owner_subject: owner,
  }, { lineWidth: 0 }).trimEnd();
  const content = `---\n${frontmatter}\n---\n\`\`\`json\n${JSON.stringify(payload, null, 2)}\n\`\`\`\n`;
  return { pageId: `markdown/instances/evolve_training_source/${id}.md`, content, payload };
}

export function preparationEvent(pageId: string, pageSha256: string): CaptureEventRequest {
  if (!/^[a-f0-9]{64}$/i.test(pageSha256))
    throw new Error('Source preparation needs the exact stored page revision.');
  const event = buildStartEvent({ skill: 'evolve_prepare_source', pageId, mode: 'run' });
  const manual = (event.provenance as { manual: Record<string, unknown> }).manual;
  manual.expected_page_sha256 = pageSha256;
  return event;
}

export function preparationReceiptId(eventId: string): string {
  return `evolve-training-source-${createHash('sha256').update(eventId).digest('hex').slice(0, 32)}-final`;
}

export function parsePreparationReceipt(
  receipt: Event, eventId: string, pageSha256: string,
): Record<string, unknown> | undefined {
  if (receipt.kind !== 'system' || receipt.source !== 'anofox-evolve'
      || receipt.label_skill !== 'evolve:training-source'
      || receipt.root_event_id !== eventId || !receipt.body) return undefined;
  let result: Record<string, unknown>;
  try { result = JSON.parse(receipt.body) as Record<string, unknown>; }
  catch { return undefined; }
  if (result.source_page_sha256 !== pageSha256) return undefined;
  return result;
}
