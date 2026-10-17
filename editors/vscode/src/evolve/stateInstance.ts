import { isHash } from './rulesModel';

/**
 * The CSV-start pilots whose instance Evolve can discover rules for (P0, P5, P7). A rule set is bound
 * to the exact instance it was tested on, so this reads the person's instance file into a closed
 * shape: only the keys each pilot reads, plus the few start keys. Anything else is refused here,
 * before a byte is sent (the service refuses it too).
 */
export type StatePilot = 'p0' | 'p5' | 'p7';

export const STATE_INSTANCE_KEYS: Readonly<Record<StatePilot, readonly string[]>> = {
  p0: ['capacity', 'items_csv'],
  p5: ['shelf_capacity', 'products_csv', 'subs_csv'],
  p7: ['max_holdings', 'max_position_size', 'assets_csv', 'covariance_csv'],
};

/** Scalars are numbers; every other instance key carries a table as text. */
const SCALARS: ReadonlySet<string> = new Set([
  'capacity',
  'shelf_capacity',
  'max_holdings',
  'max_position_size',
]);
/** Integers only; max_position_size is a fraction. */
const INTEGERS: ReadonlySet<string> = new Set(['capacity', 'shelf_capacity', 'max_holdings']);
const START_KEYS = ['experiment', 'brain', 'max_generations', 'budget'] as const;
const EXPERIMENT_ID = /^[A-Za-z0-9_-]{1,128}$/;

export const isStatePilot = (value: unknown): value is StatePilot =>
  value === 'p0' || value === 'p5' || value === 'p7';

/** Read one instance file's object; throws a message a person can act on. */
export function readStateInstance(raw: unknown): Record<string, unknown> {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw))
    throw new Error('The instance file must be one JSON object.');
  const spec = raw as Record<string, unknown>;
  if (typeof spec.pilot !== 'string') throw new Error('The instance file needs a "pilot".');
  if (!isStatePilot(spec.pilot))
    throw new Error('Rules for an instance exist for pilot p0, p5 or p7 only.');
  const keys = STATE_INSTANCE_KEYS[spec.pilot];
  const allowed = new Set<string>(['pilot', ...keys, ...START_KEYS]);
  for (const key of Object.keys(spec))
    if (!allowed.has(key))
      throw new Error(
        `The instance file has a key ${spec.pilot} does not read: ${JSON.stringify(key).slice(0, 60)}. Allowed: ${[...keys, ...START_KEYS].join(', ')}.`,
      );
  for (const key of keys) {
    const value = spec[key];
    if (value === undefined) throw new Error(`The instance file needs ${key}.`);
    if (SCALARS.has(key)) {
      const ok =
        typeof value === 'number' &&
        Number.isFinite(value) &&
        value > 0 &&
        (!INTEGERS.has(key) || Number.isSafeInteger(value));
      if (!ok)
        throw new Error(`${key} must be a positive ${INTEGERS.has(key) ? 'whole ' : ''}number.`);
    } else if (typeof value !== 'string' || value.trim() === '')
      throw new Error(`${key} must be a non-empty text table.`);
  }
  if (
    spec.experiment !== undefined &&
    !(typeof spec.experiment === 'string' && EXPERIMENT_ID.test(spec.experiment))
  )
    throw new Error('experiment must be letters, digits, - and _ (at most 128).');
  if (spec.brain !== undefined && spec.brain !== 'llm')
    throw new Error('brain must be "llm": only that brain enforces an approved rule set.');
  if (
    spec.max_generations !== undefined &&
    !(
      typeof spec.max_generations === 'number' &&
      Number.isSafeInteger(spec.max_generations) &&
      spec.max_generations >= 0
    )
  )
    throw new Error('max_generations must be a whole number, 0 or more.');
  if (
    spec.budget !== undefined &&
    !(spec.budget && typeof spec.budget === 'object' && !Array.isArray(spec.budget))
  )
    throw new Error('budget must be an object.');
  return { ...spec };
}

/** What is proposed and tested: the pilot and its instance keys, never a start key. */
export function instanceOnly(spec: Record<string, unknown>): Record<string, unknown> {
  const pilot = spec.pilot;
  if (!isStatePilot(pilot))
    throw new Error('Rules for an instance exist for pilot p0, p5 or p7 only.');
  const out: Record<string, unknown> = { pilot };
  for (const key of STATE_INSTANCE_KEYS[pilot]) out[key] = spec[key];
  return out;
}

/**
 * The start request: the same instance, the start keys from the file, brain "llm" and the approved
 * hash. The service re-derives the instance hash and the limit from this very request and refuses
 * the start if either differs from what was approved.
 */
export function stateStartRequest(
  spec: Record<string, unknown>,
  ruleSetSha256: string,
): Record<string, unknown> {
  if (!isHash(ruleSetSha256)) throw new Error('An approved rule set is a 64-character hash.');
  const read = readStateInstance(spec);
  if (typeof read.experiment !== 'string')
    throw new Error('The instance file needs a new experiment id to start one.');
  if (read.max_generations === undefined)
    throw new Error(
      'The instance file needs an explicit max_generations to start (0 runs no model call). No default is assumed.',
    );
  const request: Record<string, unknown> = {
    ...instanceOnly(read),
    experiment: read.experiment,
    max_generations: read.max_generations,
    ...(read.budget !== undefined ? { budget: read.budget } : {}),
    brain: 'llm',
    approved_rule_set_sha256: ruleSetSha256,
  };
  return request;
}
