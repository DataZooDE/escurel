/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  EvolveShapeError,
  ALPHA_CAP,
  P_MAX_CAP,
  approvable,
  describeRule,
  parseApproval,
  parsePolicyInput,
  parseProposal,
  parseTestReport,
  renderReport,
} from '../../src/evolve/rulesModel';

/**
 * Real JSON captured from a merged evolve-agent (policy v2, #44) running on the 16 SKU x 90 day
 * source of the e2e spec, and from a P0 state batch (state_replay_v1). Nothing here is hand-written
 * except hostile fields added on purpose; the old v1 record is real output of the commit before #44
 * (see fixtures/evolve-rules/test_v1_real.provenance.txt).
 */
const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));

const v2 = fx('test_v2_pmax5');
const state = fx('test_state_p0');

const blockOf = (text: string, sha: string) => {
  const parts = text.split('\n## ');
  const hit = parts.find((p) => p.includes(sha));
  if (!hit) throw new Error('rule not in the document');
  return hit;
};

describe('policy v2 report (real service output)', () => {
  const report = parseTestReport(v2);

  it('reads the policy, the caps and the bound of every rule', () => {
    expect(report.policy).toMatchObject({
      version: 'binom_holm_ub_v2',
      pMax: 0.05,
      alpha: 0.05,
      caps: { pMaxMax: 0.05, alphaMax: 0.1 },
    });
    expect(report.kind).toBe('binomial');
    for (const e of report.evidence) {
      expect(e.bound).toBeDefined();
      expect(e.bound!.method).toBe('clopper_pearson_one_sided');
      expect(e.bound!.confidence).toBe(0.95);
    }
  });

  it('shows the exact bound statement of a supported and of an insufficient_evidence rule', () => {
    const text = renderReport(report);
    const supported = v2.evidence.find((e: any) => e.decision === 'supported');
    const insufficient = v2.evidence.find((e: any) => e.decision === 'insufficient_evidence');
    expect(blockOf(text, supported.rule_sha256)).toContain(supported.bound_statement);
    expect(blockOf(text, insufficient.rule_sha256)).toContain(insufficient.bound_statement);
    expect(blockOf(text, supported.rule_sha256)).toMatch(/^Supported:/);
    expect(blockOf(text, insufficient.rule_sha256)).toMatch(/^Insufficient evidence:/);
  });

  it('shows observed breaches and rate next to the bound, as counts and percentages', () => {
    const text = renderReport(report);
    const rejected = v2.evidence.find((e: any) => e.decision === 'rejected');
    const block = blockOf(text, rejected.rule_sha256);
    expect(block).toContain(`${rejected.confirmation.breaches} of ${rejected.confirmation.rows}`);
    expect(block).toMatch(/observed breach rate: 100(\.0+)?%/);
  });

  it('says in plain words that the bound is informational and not an approval signal', () => {
    const text = renderReport(report);
    expect(text).toMatch(/informational/i);
    expect(text).toMatch(/not an approval signal/i);
    expect(text).toMatch(/never makes a rule supported/i);
    expect(text).toContain('binom_holm_ub_v2');
    expect(text).toMatch(/p_max may be up to 5%/);
    expect(text).toMatch(/alpha up to 10%/);
  });

  it('explains what each decision means, and that a low bound can still be insufficient evidence', () => {
    const text = renderReport(report);
    const insufficient = v2.evidence
      .filter((e: any) => e.decision === 'insufficient_evidence')
      .sort((a: any, b: any) => a.breach_rate_upper_bound - b.breach_rate_upper_bound)[0];
    const block = blockOf(text, insufficient.rule_sha256);
    expect(block).toContain(insufficient.bound_statement);
    expect(block).toMatch(/does not make it supported/i);
    expect(text).toMatch(/Supported means/);
    expect(text).toMatch(/Insufficient evidence means/);
    expect(text).toMatch(/Rejected means/);
  });

  it('never presents a bound as an approval signal (no tick, no "safe", no "approved")', () => {
    const text = renderReport(report);
    const boundLines = text.split('\n').filter((l) => /upper bound|breach rate is below/i.test(l));
    expect(boundLines.length).toBeGreaterThan(10);
    for (const l of boundLines)
      expect(l).not.toMatch(/✓|✔|✅|👍|🟢|\bsafe\b|\bapproved?\b|\bOK\b/i);
  });

  it('keeps a rule with a bound BELOW p_max insufficient (real service output, 400 rows at p_max 1%)', () => {
    const short = fx('test_v2_default_short');
    const low = short.evidence.find(
      (e: any) =>
        e.decision === 'insufficient_evidence' &&
        e.breach_rate_upper_bound < short.policy.p_max &&
        e.confirmation.breaches === 0,
    );
    expect(low).toBeDefined();
    const text = renderReport(parseTestReport(short));
    const block = blockOf(text, low.rule_sha256);
    expect(block).toMatch(/^Insufficient evidence:/);
    expect(block).toContain(low.bound_statement);
    expect(block).toMatch(/does not make it supported/i);
    expect(
      parseTestReport(short).evidence.find((e) => e.ruleSha256 === low.rule_sha256)!.decision,
    ).toBe('insufficient_evidence');
    // and it is not on the list of rules that can be approved
    expect(approvable(parseTestReport(short)).some((e) => e.ruleSha256 === low.rule_sha256)).toBe(
      false,
    );
  });

  it('does not change what approval is bound to', () => {
    expect(report.testSha256).toBe(v2.test_sha256);
    expect(renderReport(report)).toContain(v2.test_sha256);
    // Only the supported rules are offered, in the service order, exactly as before.
    expect(report.evidence.filter((e) => e.decision === 'supported')).toHaveLength(v2.supported);
  });

  it('ignores a bound statement that is not in the exact format, and shows its own from the numbers', () => {
    const hostile = structuredClone(v2);
    hostile.evidence[0].bound_statement = 'SECRET-ROW sku_id=7 demand=987654321';
    const text = renderReport(parseTestReport(hostile));
    expect(text).not.toContain('SECRET-ROW');
    expect(text).not.toContain('987654321');
    const e = hostile.evidence[0];
    expect(blockOf(text, e.rule_sha256)).toContain(
      `${e.confirmation.breaches} breaches in ${e.confirmation.rows} held-out rows; with 95% confidence the breach rate is below`,
    );
  });

  it('drops any field it does not name (no row value can reach the document)', () => {
    const leaky = structuredClone(v2);
    leaky.evidence.forEach((e: any) => (e.sample_rows = [{ sku_id: 7, qty: 424242 }]));
    leaky.example_values = ['SECRET-VALUE'];
    leaky.upper_bound_note = 'SECRET-VALUE in a note';
    const text = renderReport(parseTestReport(leaky));
    for (const s of ['424242', 'SECRET-VALUE', 'sample_rows']) expect(text).not.toContain(s);
  });

  it('refuses a bound outside 0..1, a confidence outside 0..1 or a missing half of the bound', () => {
    for (const patch of [
      { breach_rate_upper_bound: 1.5 },
      { breach_rate_upper_bound: -0.1 },
      { upper_bound_confidence: 2 },
      { breach_rate_observed: 'x' },
    ]) {
      const bad = structuredClone(v2);
      Object.assign(bad.evidence[0], patch);
      expect(() => parseTestReport(bad)).toThrow(EvolveShapeError);
    }
    const half = structuredClone(v2);
    delete half.evidence[0].breach_rate_upper_bound;
    expect(() => parseTestReport(half)).toThrow(EvolveShapeError);
  });

  it('notes when a chosen policy was not applied because the batch had been tested before', () => {
    const again = parseTestReport({ ...v2, already_tested: true });
    const text = renderReport(again, { requested: { pMax: 0.01, alpha: 0.05 } });
    expect(text).toMatch(/already tested/i);
    expect(text).toMatch(/your choice .* was not applied/i);
    expect(renderReport(again, { requested: { pMax: 0.05, alpha: 0.05 } })).not.toMatch(
      /was not applied/,
    );
  });
});

describe('policy v1 report (real service output of anofox-evolve 9afa37b, before policy v2)', () => {
  const v1 = fx('test_v1_real');

  it('is a genuine pre-#44 record: no policy version, no bound anywhere', () => {
    expect(v1.policy.policy_version).toBeUndefined();
    expect(v1.upper_bound_note).toBeUndefined();
    for (const e of v1.evidence) expect(e.breach_rate_upper_bound).toBeUndefined();
  });

  it('is read as binom_holm_v1 and states that no bound was computed for it', () => {
    const report = parseTestReport(v1);
    expect(report.policy.version).toBe('binom_holm_v1');
    expect(report.policy.caps).toBeUndefined();
    const text = renderReport(report);
    expect(text).toContain('binom_holm_v1');
    expect(text).toMatch(/no upper bound/i);
    expect(text).not.toContain('breach rate is below');
    expect(text).not.toMatch(/caps/i);
  });

  it('the same stored test read back through the policy-v2 service names the caps as new-test only', () => {
    const viaV2 = fx('test_v1_via_v2_service');
    expect(viaV2.test_sha256).toBe(v1.test_sha256);
    const report = parseTestReport(viaV2);
    expect(report.policy.version).toBe('binom_holm_v1');
    expect(report.policy.caps).toEqual({ pMaxMax: 0.05, alphaMax: 0.1 });
    const text = renderReport(report);
    expect(text).toMatch(/no upper bound/i);
    expect(text).toMatch(/caps apply to new tests only/i);
  });
});

describe('choosing p_max and alpha', () => {
  it('has the service caps', () => {
    expect(P_MAX_CAP).toBe(0.05);
    expect(ALPHA_CAP).toBe(0.1);
  });

  it('accepts a percentage within the cap and turns it into the fraction the service takes', () => {
    expect(parsePolicyInput('5', 'p_max')).toEqual({ ok: true, value: 0.05 });
    expect(parsePolicyInput('2.5 %', 'p_max')).toEqual({ ok: true, value: 0.025 });
    expect(parsePolicyInput('10', 'alpha')).toEqual({ ok: true, value: 0.1 });
    expect(parsePolicyInput('0,5', 'alpha')).toEqual({ ok: true, value: 0.005 });
  });

  it('rejects a value above the cap, zero, negative, not a number, or a non-finite one', () => {
    for (const [text, what] of [
      ['5.01', 'p_max'],
      ['6', 'p_max'],
      ['10.5', 'alpha'],
      ['0', 'p_max'],
      ['-1', 'alpha'],
      ['abc', 'p_max'],
      ['', 'alpha'],
      ['Infinity', 'p_max'],
      ['NaN', 'alpha'],
      ['1e-400', 'alpha'],
    ] as const) {
      const r = parsePolicyInput(text, what);
      expect(r.ok, `${text} ${what}`).toBe(false);
    }
    const over = parsePolicyInput('6', 'p_max');
    expect(over.ok === false && over.message).toMatch(/at most 5%/);
  });
});

describe('approval receipts carry the policy', () => {
  it('reads the policy of a freshly approved v2 set', () => {
    const a = parseApproval(fx('approve_v2'));
    expect(a.policy).toMatchObject({ version: 'binom_holm_ub_v2', pMax: 0.05, alpha: 0.05 });
  });

  it('reads a set approved before policies existed (policy null) without failing', () => {
    const a = parseApproval({ ...fx('approve_v2'), policy: null });
    expect(a.policy).toBeUndefined();
  });

  it('reads a state set (no policy at all)', () => {
    const a = parseApproval(fx('approve_state_p0'));
    expect(a.policy).toBeUndefined();
    expect(a.rules).toHaveLength(3);
  });
});

describe('state_replay_v1 report (real service output, P0)', () => {
  const report = parseTestReport(state);

  it('is read without a p-value, a rate or a window', () => {
    expect(report.kind).toBe('state_replay');
    expect(report.policy.version).toBe('state_replay_v1');
    expect(report.policy.pMax).toBeUndefined();
    expect(report.supported).toBe(3);
    expect(report.replays).toHaveLength(6);
  });

  it('shows what was replayed and what each control showed, as counts only', () => {
    const text = renderReport(report);
    expect(text).toContain('state_replay_v1');
    expect(text).toMatch(/replay/i);
    expect(text).toMatch(/not a statistical test/i);
    expect(text).not.toMatch(/p-value|Holm|upper bound|breach rate/i);
    for (const program of [
      'seed',
      'next_fit',
      'first_fit_decreasing',
      'single_bin',
      'fake_item_id',
      'omit_items',
    ])
      expect(text).toContain(program);
    const agg = state.evidence.find((e: any) => e.rule.kind === 'agg_ge');
    const block = blockOf(text, agg.rule_sha256);
    expect(block).toMatch(/omit_items[^\n]*1 breach/);
    expect(block).toMatch(/seed[^\n]*0 breaches/);
    expect(block).toMatch(/positive control/);
    expect(block).toMatch(/planted negative/);
  });

  it('names the guard a negative control tripped, by name only', () => {
    const text = renderReport(report);
    expect(text).toContain('over_capacity');
    expect(text).toContain('incomplete_assignment');
  });

  it('explains a replay rejection in words', () => {
    const rejected = structuredClone(state);
    rejected.evidence[0].decision = 'rejected';
    rejected.evidence[0].reason = 'negative_control_not_breached';
    rejected.supported = 2;
    rejected.rejected = 1;
    const text = renderReport(parseTestReport(rejected));
    expect(text).toMatch(/planted negative did not breach this rule/i);
    expect(text).toMatch(/holds vacuously|vacuous/i);
    rejected.evidence[0].reason = 'SECRET-VALUE sku_id=7 qty=424242';
    const hostile = renderReport(parseTestReport(rejected));
    expect(hostile).not.toContain('SECRET-VALUE');
    expect(hostile).not.toContain('424242');
    rejected.evidence[0].reason = 'a_reason_from_the_future';
    expect(renderReport(parseTestReport(rejected))).toMatch(/reason not recognised/i);
  });

  it('refuses a replay whose counts name a rule that is not in the report', () => {
    const bad = structuredClone(state);
    bad.replays[0].breaches['f'.repeat(64)] = 3;
    expect(() => parseTestReport(bad)).toThrow(EvolveShapeError);
  });

  it('reads a state proposal (no split window)', () => {
    const p = parseProposal(fx('propose_state_p0'));
    expect(p.kind).toBe('state');
    expect(p.dayCut).toBeUndefined();
    expect(p.candidates).toHaveLength(3);
  });
});

describe('the rule kinds in words', () => {
  const rules = fx('propose_v2').candidates.map((c: any) => c.rule);
  const stateRules = fx('propose_state_p0').candidates.map((c: any) => c.rule);

  it('renders the five input kinds', () => {
    expect(describeRule({ kind: 'not_null', table: 'demand', column: 'qty' })).toBe(
      'demand.qty is never empty',
    );
    expect(describeRule({ kind: 'range', table: 'demand', column: 'qty', min: 0, max: 36 })).toBe(
      'demand.qty is between 0 and 36',
    );
    expect(
      describeRule({ kind: 'in_set', table: 'sku', column: 'lead_time', values: [2, 3] }),
    ).toBe('sku.lead_time is one of 2, 3');
    expect(describeRule({ kind: 'multiple_of', table: 'demand', column: 'qty', divisor: 6 })).toBe(
      'demand.qty is a multiple of 6',
    );
    expect(describeRule({ kind: 'unique', table: 'sku', columns: ['sku_id'] })).toBe(
      'sku has no two rows with the same sku_id',
    );
    // and every kind the real service proposed reads as words, not as the fallback
    for (const r of rules) expect(describeRule(r)).not.toMatch(/details not rendered/);
  });

  it('renders fk_in', () => {
    const r = stateRules.find((x: any) => x.kind === 'fk_in');
    expect(describeRule(r)).toBe('every p0_bin_assignment.item_id exists in p0_items.id');
  });

  it('renders agg_le (grouped sum over a joined input table)', () => {
    const r = stateRules.find((x: any) => x.kind === 'agg_le');
    expect(describeRule(r)).toBe(
      'p0_bin_assignment: each bin_id group has a total p0_items.size of at most 10 (rows joined to p0_items on item_id = id)',
    );
  });

  it('renders agg_ge (ungrouped count) and its grouped, filtered, summed form', () => {
    const r = stateRules.find((x: any) => x.kind === 'agg_ge');
    expect(describeRule(r)).toBe('p0_bin_assignment: the whole table has at least 16 rows');
    expect(
      describeRule({
        kind: 'agg_ge',
        table: 'shelf',
        group_by: 'row',
        agg: 'sum',
        column: 'units',
        filter: { column: 'listed', value: true },
        bound: 3,
      }),
    ).toBe(
      'shelf: each row group has a total shelf.units of at least 3 (only rows where listed is true)',
    );
  });

  it('renders a count with a filter and a join', () => {
    expect(
      describeRule({
        kind: 'agg_le',
        table: 'a',
        agg: 'count',
        filter: { column: 'on', value: false },
        join: { table: 'b', state_column: 'k', input_column: 'id' },
        bound: 4,
      }),
    ).toBe(
      'a: the whole table has at most 4 rows (only rows where on is false; rows joined to b on k = id)',
    );
  });

  it('fails safe on a kind it does not know: the kind name and no details', () => {
    const text = describeRule({
      kind: 'agg_between',
      table: 'secret_table',
      lo: 'SECRET-VALUE',
      hi: 9,
    });
    expect(text).toBe('rule kind "agg_between": details not rendered');
    expect(text).not.toContain('SECRET-VALUE');
    expect(text).not.toContain('secret_table');
  });

  it('cleans a hostile kind name and survives a non-object rule', () => {
    expect(describeRule({ kind: 'x‮y\n[z](u)' })).not.toMatch(/[‮\n]/);
    expect(describeRule(null)).toMatch(/details not rendered/);
    expect(describeRule({ kind: 5 })).toMatch(/details not rendered/);
  });

  it('a malformed bound in a known kind does not print as a number', () => {
    expect(describeRule({ kind: 'agg_ge', table: 't', agg: 'count', bound: 'DROP TABLE' })).toBe(
      't: the whole table has at least ? rows',
    );
  });
});
