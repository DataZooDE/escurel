/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  EvolveShapeError,
  approvable,
  describeRule,
  parseApproval,
  parseProposal,
  parseTestReport,
  renderReport,
} from '../../src/evolve/rulesModel';

const H = (c: string) => c.repeat(64);

const rule = (kind: Record<string, unknown>, sha: string) => ({ rule_sha256: sha, rule: kind });

const proposal = {
  batch_id: H('1'),
  data_sha256: H('2'),
  proposer: 'deterministic_v1',
  dsl_version: 'rules_v1',
  split: { version: 'rule_split_v1', seed: H('3'), day_cut: 60, days: 90 },
  candidates: [
    rule({ kind: 'multiple_of', table: 'p1_history', column: 'demand', divisor: 6 }, H('a')),
    rule({ kind: 'range', table: 'p1_history', column: 'demand', min: 0, max: 36 }, H('b')),
    rule({ kind: 'in_set', table: 'p1_sku', column: 'lead_time', values: [2, 3] }, H('c')),
  ],
  candidates_count: 3,
  idempotent: false,
  next_action: 'evolve_rules_test',
};

const evidence = (sha: string, decision: string, rows: number, breaches: number, p: number) => ({
  rule: { kind: 'not_null', table: 't', column: 'c' },
  rule_sha256: sha,
  discovery: { rows: 100, breaches: 0 },
  confirmation: { rows, breaches },
  p_value: p,
  rows_required: 299,
  decision,
});

const tested = {
  batch_id: H('1'),
  test_sha256: H('4'),
  already_tested: false,
  policy: { p_max: 0.01, alpha: 0.05 },
  split: { version: 'rule_split_v1', seed: H('3'), day_cut: 60, days: 90 },
  candidates_before: 0,
  supported: 1,
  insufficient_evidence: 1,
  rejected: 1,
  evidence: [
    evidence(H('a'), 'supported', 480, 0, 0.0001),
    evidence(H('b'), 'insufficient_evidence', 120, 0, 0.3),
    evidence(H('c'), 'rejected', 480, 30, 0.9),
  ],
  claim: 'survived a pre-registered confirmation test',
  next_action: 'a person approves supported rules with evolve_rules_approve',
};

describe('rule discovery payloads', () => {
  it('reads a proposal and describes each rule in words', () => {
    const p = parseProposal(proposal);
    expect(p.batchId).toBe(H('1'));
    expect(p.candidates.map((c) => c.text)).toEqual([
      'p1_history.demand is a multiple of 6',
      'p1_history.demand is between 0 and 36',
      'p1_sku.lead_time is one of 2, 3',
    ]);
  });

  it('describes the other rule kinds and tolerates an unknown kind', () => {
    expect(describeRule({ kind: 'not_null', table: 't', column: 'c' })).toBe('t.c is never empty');
    expect(describeRule({ kind: 'range', table: 't', column: 'c', min: 1 })).toBe(
      't.c is at least 1',
    );
    expect(describeRule({ kind: 'range', table: 't', column: 'c', max: 9 })).toBe(
      't.c is at most 9',
    );
    expect(describeRule({ kind: 'unique', table: 't', columns: ['a', 'b'] })).toBe(
      't has no two rows with the same a, b',
    );
    expect(describeRule({ kind: 'surprise', table: 't' })).toBe(
      'rule kind "surprise": details not rendered',
    );
  });

  it('rejects a malformed proposal instead of showing it', () => {
    for (const bad of [
      null,
      [],
      { ...proposal, batch_id: 'nope' },
      { ...proposal, candidates: 'x' },
      { ...proposal, candidates: [{ rule_sha256: 'short', rule: {} }] },
    ])
      expect(() => parseProposal(bad)).toThrow(EvolveShapeError);
  });

  it('reads a test report with counts, p-values, required rows and the three decisions', () => {
    const t = parseTestReport(tested);
    expect(t.testSha256).toBe(H('4'));
    expect([t.supported, t.insufficientEvidence, t.rejected]).toEqual([1, 1, 1]);
    expect(t.evidence.map((e) => e.decision)).toEqual([
      'supported',
      'insufficient_evidence',
      'rejected',
    ]);
    expect(t.evidence[1]!.rowsRequired).toBe(299);
    expect(t.evidence[1]!.confirmation).toEqual({ rows: 120, breaches: 0 });
  });

  it('refuses a report whose counts disagree with its evidence, or with an unknown decision', () => {
    expect(() => parseTestReport({ ...tested, supported: 2 })).toThrow(EvolveShapeError);
    expect(() =>
      parseTestReport({ ...tested, evidence: [evidence(H('a'), 'maybe', 1, 0, 0.5)] }),
    ).toThrow(EvolveShapeError);
    expect(() => parseTestReport({ ...tested, test_sha256: 'x' })).toThrow(EvolveShapeError);
  });

  it('renders the report without any field it does not know (value-free)', () => {
    const leaky = {
      ...tested,
      evidence: tested.evidence.map((e) => ({
        ...e,
        sample_rows: [{ sku_id: 7, demand: 987654321 }],
        breach_values: [123456789],
      })),
      example_values: ['SECRET-VALUE'],
    };
    const text = renderReport(parseTestReport(leaky));
    for (const secret of ['987654321', '123456789', 'SECRET-VALUE', 'sample_rows'])
      expect(text).not.toContain(secret);
    expect(text).toContain('Supported');
    expect(text).toContain('Insufficient evidence');
    expect(text).toContain('299');
    expect(text).toContain('Holm');
    expect(text).toContain(H('4'));
  });

  it('explains insufficient evidence as not being a verdict on the rule', () => {
    expect(renderReport(parseTestReport(tested))).toMatch(
      /too few confirmation rows[^.]*not a verdict/i,
    );
  });

  it('only supported rules can be approved', () => {
    expect(approvable(parseTestReport(tested)).map((e) => e.ruleSha256)).toEqual([H('a')]);
    const none = parseTestReport({
      ...tested,
      supported: 0,
      insufficient_evidence: 3,
      rejected: 0,
      evidence: tested.evidence.map((e) => ({ ...e, decision: 'insufficient_evidence' })),
    });
    expect(approvable(none)).toEqual([]);
    expect(renderReport(none)).toContain('Nothing can be approved');
  });

  it('reads an approval receipt', () => {
    const a = parseApproval({
      approved_rule_set_sha256: H('9'),
      rules: [proposal.candidates[0]],
      data_sha256: H('2'),
      pilot: 'p1_decision',
      approver: 'alice',
      idempotent: false,
    });
    expect(a.ruleSetSha256).toBe(H('9'));
    expect(a.approver).toBe('alice');
    expect(() => parseApproval({ approved_rule_set_sha256: 'x' })).toThrow(EvolveShapeError);
  });

  it('cleans hostile text in a rule name before it reaches a label', () => {
    const text = describeRule({ kind: 'not_null', table: 't‮evil', column: 'c\n[x](y)' });
    expect(text).not.toContain('‮');
    expect(text).not.toContain('\n');
  });
});

describe('the parsed evidence is a closed shape (no field the service adds can pass through)', () => {
  const fx = (name: string): any =>
    JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));
  const HOSTILE = ['rows', 'example', 'values', 'rule_json', 'row_values'];
  const MARK = 'HOSTILE_MARKER_8c1f';
  const BINOMIAL = [
    'bound',
    'confirmation',
    'decision',
    'discovery',
    'pValue',
    'rowsRequired',
    'ruleSha256',
    'text',
  ];
  const STATE = ['decision', 'reason', 'ruleSha256', 'text'];
  const REPLAY = ['breaches', 'guard', 'program', 'role'];

  const poison = (raw: any) => {
    const r = structuredClone(raw);
    for (const e of r.evidence) for (const k of HOSTILE) e[k] = { [k]: MARK, leak: [MARK] };
    for (const e of r.replays ?? []) for (const k of HOSTILE) e[k] = MARK;
    return r;
  };

  for (const [name, allowed] of [
    ['test_v2_pmax5', BINOMIAL],
    ['test_state_p0', STATE],
  ] as const) {
    it(`${name}: every evidence entry has only the whitelisted keys`, () => {
      const report = parseTestReport(poison(fx(name)));
      expect(report.evidence.length).toBeGreaterThan(0);
      for (const e of report.evidence)
        for (const key of Object.keys(e)) expect(allowed).toContain(key);
      for (const r of report.replays)
        for (const key of Object.keys(r)) expect(REPLAY).toContain(key);
      expect(JSON.stringify(report)).not.toContain(MARK);
      expect(renderReport(report)).not.toContain(MARK);
    });
  }

  it('the state fixture really has replay entries to poison', () => {
    expect(parseTestReport(poison(fx('test_state_p0'))).replays.length).toBeGreaterThan(0);
  });
});
