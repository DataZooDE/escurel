import { stringify } from 'yaml';

const REQUIRED = [
  'capacity', 'skus', 'service_targets', 'seed_sql', 'baseline_sql',
  'planning_window_days', 'scored_window_days', 'unit_order_costs',
  'terminal_stock_tolerance', 'training_start', 'training_end',
  'history_start', 'history_end', 'source_sha256', 'max_generations', 'budget',
] as const;

const ALLOWED = new Set<string>([
  ...REQUIRED, 'num_islands', 'migration_interval', 'operator', 'model_tier',
  'model_ensemble', 'model_ensemble_strong_every', 'capture_rationale',
]);
const SKU_FIELDS = new Set([
  'sku_id', 'name', 'initial_stock', 'initial_pipeline', 'history', 'demand',
  'lead_time', 'case_pack', 'min_order', 'holding_cost', 'shortage_cost',
  'fixed_order_cost',
]);

function onlyKeys(value: unknown, allowed: Set<string>, label: string): void {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error(`${label} must be an object.`);
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) throw new Error(`Unsupported ${label} key ${key}.`);
  }
}

/** Smoke-only starter. The digest placeholder deliberately fails Evolve admission. */
export const v2TrainingStarter = {
  capacity: 10,
  skus: [{ sku_id: 1, name: 'REPLACE_WITH_TRAINING_SKU', initial_stock: 1,
    initial_pipeline: [0, 0, 0, 0, 0, 0], history: [1, 1],
    demand: [1, 1, 1, 1, 1, 2], lead_time: 1, case_pack: 1, min_order: 0,
    holding_cost: 0, shortage_cost: 10, fixed_order_cost: 5 }],
  service_targets: { aggregate_min_fill_rate: 0.8, per_sku_min_fill_rate: { '1': 0.8 } },
  seed_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
  baseline_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
  planning_window_days: 2, scored_window_days: 2,
  unit_order_costs: { '1': 1 }, terminal_stock_tolerance: { '1': 0 },
  training_start: '2026-08-01', training_end: '2026-08-06',
  history_start: '2026-07-30', history_end: '2026-07-31',
  source_sha256: 'REPLACE_WITH_64_HEX_TRAINING_SOURCE_SHA256',
  max_generations: 0, budget: { max_evaluated: 1 },
};

export function normalizeV2TrainingSpec(value: unknown, holdoutId: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Training spec must be one JSON object.');
  if (!holdoutId.trim() || holdoutId.length > 128 || /[\r\n\x00-\x1f]/.test(holdoutId))
    throw new Error('Enter the registered private holdout ID.');
  const input = value as Record<string, unknown>;
  for (const key of Object.keys(input)) {
    if (!ALLOWED.has(key))
      throw new Error(`Unsupported training-spec key ${key}. Keep holdout outcomes and acceptance terms in the private registry.`);
  }
  for (const key of REQUIRED) {
    if (!(key in input) || input[key] === null || input[key] === undefined)
      throw new Error(`Training spec needs ${key}.`);
  }
  for (const key of ['seed_sql', 'baseline_sql', 'training_start', 'training_end',
    'history_start', 'history_end', 'source_sha256']) {
    if (typeof input[key] !== 'string' || !(input[key] as string).trim())
      throw new Error(`Training spec needs a nonempty ${key}.`);
  }
  if (!/^[a-f0-9]{64}$/i.test(input.source_sha256 as string))
    throw new Error('Replace source_sha256 with the 64-character digest of the training source.');
  if (!Array.isArray(input.skus) || input.skus.length === 0)
    throw new Error('Training spec needs at least one SKU.');
  for (const [index, sku] of input.skus.entries()) onlyKeys(sku, SKU_FIELDS, `SKU ${index + 1}`);
  onlyKeys(input.service_targets, new Set(['aggregate_min_fill_rate', 'per_sku_min_fill_rate']), 'service_targets');
  onlyKeys(input.budget, new Set(['max_generated', 'max_evaluated', 'max_usd']), 'budget');
  if (!Number.isSafeInteger(input.capacity) || (input.capacity as number) <= 0)
    throw new Error('Training spec capacity must be a positive integer.');
  if (!Number.isSafeInteger(input.max_generations) || (input.max_generations as number) < 0)
    throw new Error('Training spec max_generations must be a nonnegative integer.');
  return {
    ...input,
    pilot: 'p1_decision',
    brain: 'llm',
    evaluator_version: 'replenishment_decision_v2',
    holdout_id: holdoutId,
  };
}

export function smokeOnlyWarnings(value: Record<string, unknown>): string[] {
  const warnings: string[] = [];
  if (value.max_generations === 0) warnings.push('zero proposal generations');
  if (typeof value.seed_sql === 'string' && typeof value.baseline_sql === 'string'
      && value.seed_sql.trim() === value.baseline_sql.trim())
    warnings.push('the seed and baseline are identical');
  return warnings;
}

export function v2ProblemPage(args: {
  id: string;
  owner: string;
  holdoutId: string;
  objective: string;
  trainingSpec: unknown;
}): { pageId: string; content: string } {
  const { id, owner, holdoutId, objective, trainingSpec } = args;
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(id))
    throw new Error('Use a lowercase page ID with letters, numbers, underscores, or hyphens.');
  if (!owner.trim()) throw new Error('Sign in before importing an owner-scoped Evolve problem.');
  const searchRequest = normalizeV2TrainingSpec(trainingSpec, holdoutId);
  const frontmatter = stringify({
    type: 'instance', skill: 'evolve_problem', id, owner_subject: owner,
    pilot: 'p1_decision', holdout_id: holdoutId, search_request: searchRequest,
  }, { lineWidth: 0 }).trimEnd();
  const heading = objective.trim() || 'V2 replenishment decision search';
  return {
    pageId: `markdown/instances/evolve_problem/${id}.md`,
    content: `---\n${frontmatter}\n---\n# ${heading.replace(/[\r\n]+/g, ' ')}\n\nTraining specification for owner review. Private holdout outcomes stay in Evolve's registry.\n`,
  };
}
