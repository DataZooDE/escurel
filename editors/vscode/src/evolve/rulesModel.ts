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
  dayCut: number;
  days: number;
  candidates: RuleView[];
}
export interface Evidence {
  ruleSha256: string;
  text: string;
  discovery: RuleCount;
  confirmation: RuleCount;
  pValue: number;
  rowsRequired: number;
  decision: Decision;
}
export interface TestReport {
  batchId: string;
  testSha256: string;
  alreadyTested: boolean;
  pMax: number;
  alpha: number;
  candidatesBefore: number;
  supported: number;
  insufficientEvidence: number;
  rejected: number;
  evidence: Evidence[];
}
export interface Approval {
  ruleSetSha256: string;
  dataSha256: string;
  approver: string;
  rules: RuleView[];
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

/** A rule in words. Names come from the training data's schema, so they are cleaned like any text. */
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
      return `${col} is a multiple of ${typeof r.divisor === 'number' ? r.divisor : '?'}`;
    case 'unique': {
      const cols = Array.isArray(r.columns) ? r.columns.map(name).join(', ') : '?';
      return `${name(r.table)} has no two rows with the same ${cols}`;
    }
    default:
      return 'an unrecognised rule kind';
  }
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
  const split = obj(p.split, 'split');
  return {
    batchId: hash(p.batch_id, 'batch id'),
    dataSha256: hash(p.data_sha256, 'data hash'),
    dayCut: count(split.day_cut, 'split'),
    days: count(split.days, 'split'),
    candidates: p.candidates.map(ruleView),
  };
}

export function parseTestReport(value: unknown): TestReport {
  const t = obj(value, 'test report');
  const policy = obj(t.policy, 'policy');
  if (!Array.isArray(t.evidence)) throw new EvolveShapeError('Evolve returned no evidence.');
  const evidence: Evidence[] = t.evidence.map((raw) => {
    const e = obj(raw, 'evidence');
    const decision = DECISIONS.find((d) => d === e.decision);
    if (!decision) throw new EvolveShapeError('Evolve returned an unknown decision.');
    return {
      ruleSha256: hash(e.rule_sha256, 'rule hash'),
      text: describeRule(e.rule),
      discovery: ruleCount(e.discovery, 'discovery count'),
      confirmation: ruleCount(e.confirmation, 'confirmation count'),
      pValue: real(e.p_value, 'p-value'),
      rowsRequired: count(e.rows_required, 'required rows'),
      decision,
    };
  });
  const report: TestReport = {
    batchId: hash(t.batch_id, 'batch id'),
    testSha256: hash(t.test_sha256, 'test hash'),
    alreadyTested: t.already_tested === true,
    pMax: real(policy.p_max, 'tolerance'),
    alpha: real(policy.alpha, 'alpha'),
    candidatesBefore: count(t.candidates_before, 'earlier candidates'),
    supported: count(t.supported, 'supported count'),
    insufficientEvidence: count(t.insufficient_evidence, 'insufficient count'),
    rejected: count(t.rejected, 'rejected count'),
    evidence,
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
  return {
    ruleSetSha256: hash(a.approved_rule_set_sha256, 'rule-set hash'),
    dataSha256: hash(a.data_sha256, 'data hash'),
    approver: cleanText(typeof a.approver === 'string' ? a.approver : '', 120),
    rules: a.rules.map(ruleView),
  };
}

export const approvable = (report: TestReport): Evidence[] =>
  report.evidence.filter((e) => e.decision === 'supported');

const LABEL: Record<Decision, string> = {
  supported: 'Supported',
  insufficient_evidence: 'Insufficient evidence',
  rejected: 'Rejected',
};

/** The review document: plain text, one block per rule, nothing but counts, p-values and hashes. */
export function renderReport(report: TestReport): string {
  const total = report.candidatesBefore + report.evidence.length;
  const lines = [
    '# Discovered rules: confirmation test',
    '',
    `Test hash (the approval is bound to exactly this): ${report.testSha256}`,
    `Batch: ${report.batchId}`,
    '',
    `Each rule was tested once on the confirmation window: an exact one-sided binomial test of "the breach rate is at or above ${report.pMax}", alpha ${report.alpha}, Holm-corrected over ${total} candidate${total === 1 ? '' : 's'} (${report.candidatesBefore} tested before this batch).`,
    `Result: ${report.supported} supported, ${report.insufficientEvidence} insufficient evidence, ${report.rejected} rejected.`,
    '',
  ];
  for (const e of report.evidence) {
    lines.push(
      `## ${LABEL[e.decision]}: ${e.text}`,
      `  rule ${e.ruleSha256}`,
      `  discovery window: ${e.discovery.rows} rows, ${e.discovery.breaches} breaches`,
      `  confirmation window: ${e.confirmation.rows} rows, ${e.confirmation.breaches} breaches`,
      `  p-value ${e.pValue}; rows required to prove it at this tolerance: ${e.rowsRequired}`,
    );
    if (e.decision === 'insufficient_evidence')
      lines.push(
        '  Too few confirmation rows to prove this rule at this tolerance however clean they were: this is not a verdict on the rule.',
      );
    lines.push('');
  }
  if (!approvable(report).length)
    lines.push('Nothing can be approved: no rule was supported by this test.', '');
  lines.push(
    'Supported means the rule survived a pre-registered test on this training data. It is not a law of the world and was not tested on the sealed holdout. No row values are shown or stored with this evidence.',
  );
  return lines.join('\n') + '\n';
}
