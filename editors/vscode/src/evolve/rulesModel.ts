import { cleanText } from '../shared/untrustedText';

/**
 * What the rule-discovery tools return, read into a closed shape.
 *
 * Evolve's contract is value-free: counts, p-values, hashes and the rules themselves, never a row.
 * This reads only the fields it names; anything else the service might add (now or in a later
 * version) is dropped here and so can never reach a dialog, a document or a log line.
 */
export class EvolveShapeError extends Error {}

export type Decision = 'supported' | 'insufficient_evidence' | 'rejected';
export interface RuleCount {
  rows: number;
  breaches: number;
}
export interface RuleView {
  sha256: string;
  text: string;
}
export interface Proposal {
  batchId: string;
  dataSha256: string;
  /** A training-source batch is tested statistically; a state batch (P0/P5/P7) by replay. */
  kind: 'training_source' | 'state';
  dayCut?: number;
  days?: number;
  candidates: RuleView[];
}

/** The service's own limits on the choosable test policy (policy v2). Also enforced by the service. */
export const P_MAX_CAP = 0.05;
export const ALPHA_CAP = 0.1;
export const POLICY_V1 = 'binom_holm_v1';
export const STATE_REPLAY = 'state_replay_v1';

export interface PolicyCaps {
  pMaxMax: number;
  alphaMax: number;
}
export interface Policy {
  version: string;
  pMax?: number;
  alpha?: number;
  caps?: PolicyCaps;
}
/** What the caller chose at test time; a missing field means the service default. */
export interface PolicyChoice {
  pMax?: number;
  alpha?: number;
}
/**
 * The exact one-sided upper confidence bound on a rule's breach rate (policy v2). Informational: it
 * is shown, never used to decide, filter or pre-select anything.
 */
export interface Bound {
  observed: number;
  upper: number;
  confidence: number;
  method: string;
  statement: string;
}
export interface Evidence {
  ruleSha256: string;
  text: string;
  decision: Decision;
  /** State replay only: why a rule was not supported (a service code). */
  reason?: string;
  discovery?: RuleCount;
  confirmation?: RuleCount;
  pValue?: number;
  rowsRequired?: number;
  bound?: Bound;
}
export interface Replay {
  program: string;
  role: 'positive' | 'negative';
  /** Name of the first skill guard the program tripped, if any. */
  guard?: string;
  breaches: Record<string, number>;
}
export interface TestReport {
  batchId: string;
  testSha256: string;
  alreadyTested: boolean;
  kind: 'binomial' | 'state_replay';
  policy: Policy;
  candidatesBefore?: number;
  supported: number;
  insufficientEvidence: number;
  rejected: number;
  evidence: Evidence[];
  replays: Replay[];
}
export interface Approval {
  ruleSetSha256: string;
  dataSha256: string;
  approver: string;
  rules: RuleView[];
  /** The policy the set was tested under; undefined for sets approved before policies were recorded. */
  policy?: Policy;
}

const HASH = /^[a-f0-9]{64}$/;
const DECISIONS: readonly Decision[] = ['supported', 'insufficient_evidence', 'rejected'];

export const isHash = (value: unknown): value is string =>
  typeof value === 'string' && HASH.test(value);

function obj(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new EvolveShapeError(`Evolve returned an unreadable ${what}.`);
  return value as Record<string, unknown>;
}
function hash(value: unknown, what: string): string {
  if (!isHash(value)) throw new EvolveShapeError(`Evolve returned an invalid ${what}.`);
  return value;
}
function count(value: unknown, what: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0)
    throw new EvolveShapeError(`Evolve returned an invalid ${what}.`);
  return value;
}
function real(value: unknown, what: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value))
    throw new EvolveShapeError(`Evolve returned an invalid ${what}.`);
  return value;
}
const name = (value: unknown): string => cleanText(typeof value === 'string' ? value : '?', 80);

const num = (value: unknown): string =>
  typeof value === 'number' && Number.isFinite(value) ? String(value) : '?';
const int = (value: unknown): string =>
  typeof value === 'number' && Number.isSafeInteger(value) ? String(value) : '?';

function describeAgg(r: Record<string, unknown>, lower: boolean): string | undefined {
  const table = name(r.table);
  const join =
    r.join && typeof r.join === 'object' ? (r.join as Record<string, unknown>) : undefined;
  const limit = `${lower ? 'at least' : 'at most'} ${int(r.bound)}`;
  const group =
    typeof r.group_by === 'string' ? `each ${name(r.group_by)} group has` : 'the whole table has';
  let amount: string;
  if (r.agg === 'count') amount = `${limit} rows`;
  else if (r.agg === 'sum')
    amount = `a total ${join ? name(join.table) : table}.${name(r.column)} of ${limit}`;
  else return undefined;
  const notes: string[] = [];
  if (r.filter && typeof r.filter === 'object') {
    const f = r.filter as Record<string, unknown>;
    notes.push(
      `only rows where ${name(f.column)} is ${f.value === true ? 'true' : f.value === false ? 'false' : '?'}`,
    );
  }
  if (join)
    notes.push(
      `rows joined to ${name(join.table)} on ${name(join.state_column)} = ${name(join.input_column)}`,
    );
  return `${table}: ${group} ${amount}${notes.length ? ` (${notes.join('; ')})` : ''}`;
}

/**
 * A rule in words. Names come from the training data's schema, so they are cleaned like any text.
 * A kind this version does not know is named and nothing else of it is shown: its fields could be
 * anything, so they are not rendered.
 */
export function describeRule(rule: unknown): string {
  const r = (rule && typeof rule === 'object' ? rule : {}) as Record<string, unknown>;
  const col = `${name(r.table)}.${name(r.column)}`;
  switch (r.kind) {
    case 'not_null':
      return `${col} is never empty`;
    case 'range': {
      const min = typeof r.min === 'number' ? r.min : undefined;
      const max = typeof r.max === 'number' ? r.max : undefined;
      if (min !== undefined && max !== undefined) return `${col} is between ${min} and ${max}`;
      if (min !== undefined) return `${col} is at least ${min}`;
      if (max !== undefined) return `${col} is at most ${max}`;
      return `${col} has a range`;
    }
    case 'in_set': {
      const values = Array.isArray(r.values) ? r.values : [];
      const shown = values
        .slice(0, 16)
        .map((v) => (typeof v === 'number' ? String(v) : name(v)))
        .join(', ');
      return `${col} is one of ${shown}${values.length > 16 ? ', …' : ''}`;
    }
    case 'multiple_of':
      return `${col} is a multiple of ${num(r.divisor)}`;
    case 'unique': {
      const cols = Array.isArray(r.columns) ? r.columns.map(name).join(', ') : '?';
      return `${name(r.table)} has no two rows with the same ${cols}`;
    }
    case 'fk_in':
      return `every ${col} exists in ${name(r.ref_table)}.${name(r.ref_column)}`;
    case 'agg_le':
    case 'agg_ge': {
      const words = describeAgg(r, r.kind === 'agg_ge');
      if (words) return words;
      break;
    }
  }
  const kind = typeof r.kind === 'string' ? cleanText(r.kind, 40) : '?';
  return `rule kind "${kind}": details not rendered`;
}

function ruleView(entry: unknown): RuleView {
  const e = obj(entry, 'rule');
  return { sha256: hash(e.rule_sha256, 'rule hash'), text: describeRule(e.rule) };
}

function ruleCount(value: unknown, what: string): RuleCount {
  const c = obj(value, what);
  return { rows: count(c.rows, `${what} rows`), breaches: count(c.breaches, `${what} breaches`) };
}

export function parseProposal(value: unknown): Proposal {
  const p = obj(value, 'proposal');
  if (!Array.isArray(p.candidates))
    throw new EvolveShapeError('Evolve returned no candidate list.');
  if (p.candidates.length > 200) throw new EvolveShapeError('Evolve returned too many candidates.');
  const base = {
    batchId: hash(p.batch_id, 'batch id'),
    dataSha256: hash(p.data_sha256, 'data hash'),
    candidates: p.candidates.map(ruleView),
  };
  // A state batch (P0/P5/P7) is an experiment instance with replay evidence: it has no time split.
  if (p.split === undefined && typeof p.pilot === 'string') return { ...base, kind: 'state' };
  const split = obj(p.split, 'split');
  return {
    ...base,
    kind: 'training_source',
    dayCut: count(split.day_cut, 'split'),
    days: count(split.days, 'split'),
  };
}

function caps(value: unknown): PolicyCaps | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const c = value as Record<string, unknown>;
  return typeof c.p_max_max === 'number' &&
    Number.isFinite(c.p_max_max) &&
    typeof c.alpha_max === 'number' &&
    Number.isFinite(c.alpha_max)
    ? { pMaxMax: c.p_max_max, alphaMax: c.alpha_max }
    : undefined;
}

/** A policy record, read leniently (it is context, not a decision). Undefined when absent or unreadable. */
function lenientPolicy(value: unknown, withCaps?: unknown): Policy | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const p = value as Record<string, unknown>;
  if (p.kind === STATE_REPLAY) return { version: STATE_REPLAY };
  if (typeof p.p_max !== 'number' || typeof p.alpha !== 'number') return undefined;
  if (!Number.isFinite(p.p_max) || !Number.isFinite(p.alpha)) return undefined;
  return {
    version: typeof p.policy_version === 'string' ? cleanText(p.policy_version, 40) : POLICY_V1,
    pMax: p.p_max,
    alpha: p.alpha,
    ...(caps(withCaps) ? { caps: caps(withCaps)! } : {}),
  };
}

const BOUND_FIELDS = [
  'breach_rate_observed',
  'breach_rate_upper_bound',
  'upper_bound_confidence',
  'upper_bound_method',
  'bound_statement',
] as const;

const pct = (x: number, digits = 3): string => `${(x * 100).toFixed(digits)}%`;
/** A rate or level as a person writes it: 0.05 is 5%, 0.025 is 2.5%. */
export const percent = (x: number): string => `${+(x * 100).toPrecision(6)}%`;

const STATEMENT =
  /^(\d+) breaches in (\d+) held-out rows; with (\d+)% confidence the breach rate is below (\d+(?:\.\d+)?)%$/;

function parseBound(e: Record<string, unknown>, confirmation: RuleCount): Bound | undefined {
  const present = BOUND_FIELDS.filter((k) => e[k] !== undefined && e[k] !== null);
  if (present.length === 0) return undefined;
  if (present.length !== BOUND_FIELDS.length)
    throw new EvolveShapeError('Evolve returned an incomplete upper bound.');
  const unit = (v: unknown, what: string, open = false): number => {
    const x = real(v, what);
    if (x > 1 || x < 0 || (open && x === 0))
      throw new EvolveShapeError(`Evolve returned an invalid ${what}.`);
    return x;
  };
  const observed = unit(e.breach_rate_observed, 'observed breach rate');
  const upper = unit(e.breach_rate_upper_bound, 'upper bound');
  const confidence = unit(e.upper_bound_confidence, 'bound confidence', true);
  const method = cleanText(
    typeof e.upper_bound_method === 'string' ? e.upper_bound_method : '?',
    60,
  );
  // The service's sentence is shown only when it has exactly the documented shape AND says what the
  // counts say; anything else is replaced by a sentence built here from the numbers. A free-text
  // field is therefore never a way to put anything on the page.
  const given = typeof e.bound_statement === 'string' ? STATEMENT.exec(e.bound_statement) : null;
  const consistent =
    given &&
    Number(given[1]) === confirmation.breaches &&
    Number(given[2]) === confirmation.rows &&
    Number(given[3]) === Math.round(confidence * 100);
  const statement = consistent
    ? (e.bound_statement as string)
    : `${confirmation.breaches} breaches in ${confirmation.rows} held-out rows; with ${Math.round(confidence * 100)}% confidence the breach rate is below ${pct(upper)}`;
  return { observed, upper, confidence, method, statement };
}

function parseReplays(value: unknown, rules: Set<string>): Replay[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.length > 64)
    throw new EvolveShapeError('Evolve returned unreadable replays.');
  return value.map((raw) => {
    const r = obj(raw, 'replay');
    if (r.role !== 'positive' && r.role !== 'negative')
      throw new EvolveShapeError('Evolve returned an unknown replay role.');
    const given = obj(r.breaches, 'replay counts');
    const breaches: Record<string, number> = {};
    for (const [sha, n] of Object.entries(given)) {
      if (!rules.has(sha))
        throw new EvolveShapeError('Evolve returned a replay for an unknown rule.');
      breaches[sha] = count(n, 'replay count');
    }
    return {
      program: cleanText(typeof r.program === 'string' ? r.program : '?', 60),
      role: r.role,
      ...(typeof r.skill_guard_breach === 'string'
        ? { guard: cleanText(r.skill_guard_breach, 60) }
        : {}),
      breaches,
    };
  });
}

export function parseTestReport(value: unknown): TestReport {
  const t = obj(value, 'test report');
  const rawPolicy = obj(t.policy, 'policy');
  const state = rawPolicy.kind === STATE_REPLAY;
  if (!Array.isArray(t.evidence)) throw new EvolveShapeError('Evolve returned no evidence.');
  const evidence: Evidence[] = t.evidence.map((raw) => {
    const e = obj(raw, 'evidence');
    const decision = DECISIONS.find((d) => d === e.decision);
    if (!decision) throw new EvolveShapeError('Evolve returned an unknown decision.');
    const base = {
      ruleSha256: hash(e.rule_sha256, 'rule hash'),
      text: describeRule(e.rule),
      decision,
    };
    if (state)
      return {
        ...base,
        ...(typeof e.reason === 'string' ? { reason: cleanText(e.reason, 60) } : {}),
      };
    const confirmation = ruleCount(e.confirmation, 'confirmation count');
    const bound = parseBound(e, confirmation);
    return {
      ...base,
      discovery: ruleCount(e.discovery, 'discovery count'),
      confirmation,
      pValue: real(e.p_value, 'p-value'),
      rowsRequired: count(e.rows_required, 'required rows'),
      ...(bound ? { bound } : {}),
    };
  });
  let policy: Policy;
  if (state) policy = { version: STATE_REPLAY };
  else {
    const read = lenientPolicy(
      {
        ...rawPolicy,
        p_max: real(rawPolicy.p_max, 'tolerance'),
        alpha: real(rawPolicy.alpha, 'alpha'),
      },
      t.policy_caps,
    );
    if (!read) throw new EvolveShapeError('Evolve returned an unreadable policy.');
    policy = read;
  }
  const report: TestReport = {
    batchId: hash(t.batch_id, 'batch id'),
    testSha256: hash(t.test_sha256, 'test hash'),
    alreadyTested: t.already_tested === true,
    kind: state ? 'state_replay' : 'binomial',
    policy,
    ...(state ? {} : { candidatesBefore: count(t.candidates_before, 'earlier candidates') }),
    supported: count(t.supported, 'supported count'),
    insufficientEvidence: count(t.insufficient_evidence, 'insufficient count'),
    rejected: count(t.rejected, 'rejected count'),
    evidence,
    replays: parseReplays(t.replays, new Set(evidence.map((e) => e.ruleSha256))),
  };
  const tally = (d: Decision) => evidence.filter((e) => e.decision === d).length;
  if (
    tally('supported') !== report.supported ||
    tally('insufficient_evidence') !== report.insufficientEvidence ||
    tally('rejected') !== report.rejected
  )
    throw new EvolveShapeError('Evolve returned counts that do not match its evidence.');
  return report;
}

export function parseApproval(value: unknown): Approval {
  const a = obj(value, 'approval');
  if (!Array.isArray(a.rules) || a.rules.length === 0)
    throw new EvolveShapeError('Evolve returned an approval without rules.');
  const policy = lenientPolicy(a.policy);
  return {
    ruleSetSha256: hash(a.approved_rule_set_sha256, 'rule-set hash'),
    dataSha256: hash(a.data_sha256, 'data hash'),
    approver: cleanText(typeof a.approver === 'string' ? a.approver : '', 120),
    rules: a.rules.map(ruleView),
    ...(policy ? { policy } : {}),
  };
}

export const approvable = (report: TestReport): Evidence[] =>
  report.evidence.filter((e) => e.decision === 'supported');

/** The policy in a few words, for a modal or a notification. */
export function describePolicy(policy: Policy): string {
  if (policy.version === STATE_REPLAY)
    return 'state_replay_v1 (replay evidence, no statistical test)';
  return `${policy.version} (p_max ${percent(policy.pMax ?? NaN)}, alpha ${percent(policy.alpha ?? NaN)})`;
}

/**
 * Read a typed percentage for p_max or alpha. The service takes fractions, so "5" becomes 0.05. A
 * value above the service's cap is refused here AND by the service; this is the quick, early copy.
 */
export function parsePolicyInput(
  text: string,
  which: 'p_max' | 'alpha',
): { ok: true; value: number } | { ok: false; message: string } {
  const cap = which === 'p_max' ? P_MAX_CAP : ALPHA_CAP;
  const typed = text.trim().replace(/\s*%$/, '').replace(',', '.');
  if (!/^\d+(\.\d+)?$/.test(typed))
    return { ok: false, message: `Enter ${which} as a percentage, for example 5.` };
  const percentValue = Number(typed);
  const value = Number((percentValue / 100).toPrecision(12));
  if (!Number.isFinite(value) || value <= 0)
    return { ok: false, message: `${which} must be greater than 0%.` };
  if (value > cap)
    return { ok: false, message: `${which} must be at most ${percent(cap)} (the service's cap).` };
  return { ok: true, value };
}

const LABEL: Record<Decision, string> = {
  supported: 'Supported',
  insufficient_evidence: 'Insufficient evidence',
  rejected: 'Rejected',
};

const REASON: Record<string, string> = {
  too_few_positive_controls: 'fewer than two positive controls were replayed',
  positive_control_breached: 'a positive control (or a skill guard) breached it',
  no_negative_control: 'no planted negative exists for this rule',
  negative_control_not_breached:
    'its planted negative did not breach this rule, so the limit cannot be violated on this instance and holds vacuously',
  negative_control_breaches_other_rule: 'its planted negative also breached another candidate',
};

const plural = (n: number, one: string, many = one + 's') => `${n} ${n === 1 ? one : many}`;

/** One line on what a rule showed, for a choice list or a confirmation. Counts and p-values only. */
export function evidenceFacts(e: Evidence): string {
  if (!e.confirmation) return 'replay evidence';
  return `${e.confirmation.rows} confirmation rows, ${plural(e.confirmation.breaches, 'breach', 'breaches')}, p ${e.pValue}`;
}

function binomialBlock(e: Evidence, policy: Policy): string[] {
  const c = e.confirmation!;
  const lines = [
    `## ${LABEL[e.decision]}: ${e.text}`,
    `  rule ${e.ruleSha256}`,
    `  discovery window: ${e.discovery!.rows} rows, ${e.discovery!.breaches} breaches`,
    `  confirmation window: ${c.rows} rows, ${c.breaches} breaches`,
    `  p-value ${e.pValue}; rows required to prove it at this tolerance: ${e.rowsRequired}`,
  ];
  if (e.bound) {
    lines.push(
      `  observed breach rate: ${pct(e.bound.observed)} (${c.breaches} of ${c.rows} confirmation rows)`,
      `  upper bound (informational, not an approval signal): ${e.bound.statement}`,
    );
  } else if (policy.version === POLICY_V1) {
    lines.push('  no upper bound was computed: this test used the older policy binom_holm_v1');
  }
  if (e.decision === 'insufficient_evidence') {
    lines.push(
      '  Too few confirmation rows to prove this rule at this tolerance however clean they were: this is not a verdict on the rule.',
    );
    if (e.bound)
      lines.push(
        '  A low upper bound does not make it supported: only the test above decides, and it needs more rows.',
      );
  }
  return lines;
}

const guardNote = (r: Replay) =>
  r.guard ? `; skill guard breached: ${r.guard}` : '; no skill guard breached';

function stateBlock(e: Evidence, replays: Replay[]): string[] {
  const lines = [`## ${LABEL[e.decision]}: ${e.text}`, `  rule ${e.ruleSha256}`];
  for (const r of replays) {
    const n = r.breaches[e.ruleSha256];
    lines.push(
      `  ${r.role === 'positive' ? 'positive control' : 'planted negative'} ${r.program}: ${n === undefined ? 'no count' : plural(n, 'breach', 'breaches')} of this rule${guardNote(r)}`,
    );
  }
  if (e.decision !== 'supported')
    lines.push(
      `  not supported because: ${e.reason ? (REASON[e.reason] ?? 'reason not recognised') : 'reason not given'}`,
    );
  return lines;
}

/**
 * The review document: plain text, one block per rule, nothing but counts, rates, p-values, bounds
 * and hashes. `requested` is what the person chose before the test, if anything: it is only compared
 * with what the service applied, to say so when a batch had been tested before and the choice did
 * not take effect.
 */
export function renderReport(report: TestReport, opts: { requested?: PolicyChoice } = {}): string {
  const head = [
    report.kind === 'state_replay'
      ? '# Discovered rules: replay test'
      : '# Discovered rules: confirmation test',
    '',
    `Test hash (the approval is bound to exactly this): ${report.testSha256}`,
    `Batch: ${report.batchId}`,
    `Test policy: ${describePolicy(report.policy)}`,
  ];
  const lines = head;
  if (report.kind === 'state_replay') {
    lines.push(
      '',
      'This is replay evidence, not a statistical test: known programs were replayed on this exact instance. A rule is supported only if every positive control stayed within it and its own planted negative broke it (and no other candidate). The numbers below are counts of breaches per replayed program; no values, rows or program text are shown.',
      `Result: ${report.supported} supported, ${report.rejected} rejected.`,
      '',
    );
    for (const e of report.evidence) lines.push(...stateBlock(e, report.replays), '');
  } else {
    const before = report.candidatesBefore ?? 0;
    const total = before + report.evidence.length;
    const { policy } = report;
    if (policy.caps)
      lines.push(
        policy.version === POLICY_V1
          ? `The caps apply to new tests only (p_max up to ${percent(policy.caps.pMaxMax)}, alpha up to ${percent(policy.caps.alphaMax)}); this older test predates them.`
          : `Caps for new tests: p_max may be up to ${percent(policy.caps.pMaxMax)}, alpha up to ${percent(policy.caps.alphaMax)}.`,
      );
    const asked = opts.requested;
    if (
      report.alreadyTested &&
      asked &&
      ((asked.pMax !== undefined && asked.pMax !== policy.pMax) ||
        (asked.alpha !== undefined && asked.alpha !== policy.alpha))
    )
      lines.push(
        `This batch was already tested, so your choice (p_max ${asked.pMax === undefined ? 'default' : percent(asked.pMax)}, alpha ${asked.alpha === undefined ? 'default' : percent(asked.alpha)}) was not applied: the stored test above stands. A batch is tested once and cannot be redone.`,
      );
    lines.push(
      '',
      `Each rule was tested once on the confirmation window: an exact one-sided binomial test of "the breach rate is at or above ${policy.pMax}", alpha ${policy.alpha}, Holm-corrected over ${total} candidate${total === 1 ? '' : 's'} (${before} tested before this batch).`,
      `Result: ${report.supported} supported, ${report.insufficientEvidence} insufficient evidence, ${report.rejected} rejected.`,
      '',
      'What the decisions mean:',
      '  Supported means the rule survived the corrected test: it is the only decision that can be approved.',
      '  Insufficient evidence means the confirmation window has too few rows to prove the rule at this tolerance, however clean those rows were. It is not a verdict on the rule.',
      '  Rejected means there were enough rows and the data does not support the rule.',
    );
    if (report.evidence.some((e) => e.bound))
      lines.push(
        '',
        "The upper bound under each rule is informational. It is a one-sided exact (Clopper-Pearson) upper confidence bound on the rule's breach rate, from the confirmation window only. It is not an approval signal: it never makes a rule supported, and a rule can have a low bound and still be insufficient evidence. Only the corrected test above decides.",
      );
    lines.push('');
    for (const e of report.evidence) lines.push(...binomialBlock(e, policy), '');
  }
  if (!approvable(report).length)
    lines.push('Nothing can be approved: no rule was supported by this test.', '');
  lines.push(
    report.kind === 'state_replay'
      ? 'Supported means the rule held on this instance under the replays above. It is not a law of the world. No row values are shown or stored with this evidence.'
      : 'Supported means the rule survived a pre-registered test on this training data. It is not a law of the world and was not tested on the sealed holdout. No row values are shown or stored with this evidence.',
  );
  return lines.join('\n') + '\n';
}
