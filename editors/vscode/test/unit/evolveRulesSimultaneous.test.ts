/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { parseTestReport, renderReport } from '../../src/evolve/rulesModel';

/**
 * Real output of the evolve-agent at origin/main aff2aaf (PR #46): the simultaneous (family-wise,
 * Bonferroni) upper bound next to the per-rule one. Fixture provenance: fixtures/evolve-rules/
 * test_v1_real.provenance.txt. Only the hostile edits are made by hand.
 */
const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));

const sim = fx('test_v2_simultaneous');
const blockOf = (text: string, sha: string) => {
  const hit = text.split('\n## ').find((p) => p.includes(sha));
  if (!hit) throw new Error('rule not in the document');
  return hit;
};
const supported = sim.evidence.find((e: any) => e.decision === 'supported');
const insufficient = sim.evidence.find((e: any) => e.decision === 'insufficient_evidence');
const rejected = sim.evidence.find((e: any) => e.decision === 'rejected');

describe('simultaneous bound (real service output)', () => {
  const report = parseTestReport(sim);
  const text = renderReport(report);

  it('the fixture really carries the new fields, for m = candidates tested', () => {
    expect(supported.upper_bound_simultaneous_method).toBe('clopper_pearson_one_sided_bonferroni');
    expect(supported.simultaneous_bound_statement).toMatch(/across all 42 rules/);
    expect(sim.candidates_before + sim.evidence.length).toBe(42);
  });

  it('reads the simultaneous bound of every rule into the closed shape', () => {
    for (const e of report.evidence) {
      expect(e.simultaneous).toBeDefined();
      expect(e.simultaneous!.method).toBe('clopper_pearson_one_sided_bonferroni');
      expect(e.simultaneous!.confidence).toBe(0.95);
    }
    const s = report.evidence.find((e) => e.ruleSha256 === supported.rule_sha256)!;
    expect(s.simultaneous!.upper).toBe(supported.breach_rate_upper_bound_simultaneous);
    expect(s.simultaneous!.upper).toBeGreaterThanOrEqual(s.bound!.upper);
  });

  it('shows the exact service sentence for a supported and an insufficient rule', () => {
    expect(blockOf(text, supported.rule_sha256)).toContain(supported.simultaneous_bound_statement);
    expect(blockOf(text, insufficient.rule_sha256)).toContain(
      insufficient.simultaneous_bound_statement,
    );
    expect(blockOf(text, rejected.rule_sha256)).toContain(rejected.simultaneous_bound_statement);
  });

  it('keeps the per-rule bound beside it, labelled "this rule alone"', () => {
    const block = blockOf(text, supported.rule_sha256);
    expect(block).toContain(supported.bound_statement);
    expect(block).toMatch(/this rule alone/i);
    expect(block).toMatch(/all rules of this batch at once/i);
    const lines = block.split('\n');
    const own = lines.findIndex((l) => l.includes(supported.bound_statement));
    const all = lines.findIndex((l) => l.includes(supported.simultaneous_bound_statement));
    expect(own).toBeGreaterThan(-1);
    expect(all).toBe(own + 1);
    expect(lines[own]).toMatch(/this rule alone/i);
    expect(lines[all]).toMatch(/all rules of this batch at once/i);
  });

  it('says in the document that neither bound is an approval signal or makes a rule supported', () => {
    expect(text).toMatch(/simultaneous bound/i);
    expect(text).toMatch(/both bounds are informational/i);
    expect(text).toMatch(/neither is an approval signal/i);
    expect(text).toMatch(/neither makes a rule supported/i);
    expect(text).toMatch(/not an approval signal/i);
  });

  it('never presents the simultaneous bound as an approval signal (no tick, no "safe", no "approved")', () => {
    const lines = text.split('\n').filter((l) => /simultaneous|family-wise/i.test(l));
    expect(lines.length).toBeGreaterThan(40);
    for (const l of lines) expect(l).not.toMatch(/✓|✔|✅|👍|🟢|\bsafe\b|\bapproved?\b|\bOK\b/i);
  });

  it('a rule with a low simultaneous bound but too few rows stays insufficient and unapprovable', () => {
    const low = sim.evidence
      .filter((e: any) => e.decision === 'insufficient_evidence')
      .sort(
        (a: any, b: any) =>
          a.breach_rate_upper_bound_simultaneous - b.breach_rate_upper_bound_simultaneous,
      )[0];
    const block = blockOf(text, low.rule_sha256);
    expect(block).toMatch(/^Insufficient evidence:/);
    expect(block).toMatch(/does not make it supported/i);
  });

  it('does not change what approval is bound to or the decisions', () => {
    expect(report.testSha256).toBe(sim.test_sha256);
    expect(report.supported).toBe(sim.supported);
    expect(report.evidence.map((e) => e.decision)).toEqual(
      sim.evidence.map((e: any) => e.decision),
    );
  });
});

describe('simultaneous bound: shown text is whitelisted', () => {
  const rebuilt = (k: number, n: number, m: number, p: number, x: string) =>
    `${k} breaches in ${n} held-out rows; across all ${m} rules tested on this data, with ${p}% family-wise confidence the breach rate is below ${x}%`;

  it('rebuilds the sentence from the numbers when the service text is hostile', () => {
    const hostile = structuredClone(sim);
    hostile.evidence[0].simultaneous_bound_statement = 'SECRET-ROW sku_id=7 demand=987654321';
    const out = renderReport(parseTestReport(hostile));
    expect(out).not.toContain('SECRET-ROW');
    expect(out).not.toContain('987654321');
    const e = hostile.evidence[0];
    expect(blockOf(out, e.rule_sha256)).toContain(
      rebuilt(
        e.confirmation.breaches,
        e.confirmation.rows,
        42,
        95,
        (e.breach_rate_upper_bound_simultaneous * 100).toFixed(3),
      ),
    );
  });

  it('rebuilds the sentence when the format is right but the counts, m or level are wrong', () => {
    const e0 = sim.evidence[0];
    const good = e0.simultaneous_bound_statement as string;
    for (const bad of [
      good.replace(/^\d+ breaches/, '9 breaches'),
      good.replace(/in \d+ held-out/, 'in 5 held-out'),
      good.replace('all 42 rules', 'all 2 rules'),
      good.replace('with 95%', 'with 99%'),
      good.replace(/below [\d.]+%/, 'below 0.001%'),
      good + ' SECRET-TAIL',
    ]) {
      const hostile = structuredClone(sim);
      hostile.evidence[0].simultaneous_bound_statement = bad;
      const out = renderReport(parseTestReport(hostile));
      expect(out).not.toContain('SECRET-TAIL');
      expect(out).not.toContain(bad);
      expect(blockOf(out, e0.rule_sha256)).toContain(good);
    }
  });

  it('shows no simultaneous line at all when a v2 report lacks the fields', () => {
    const old = structuredClone(sim);
    for (const e of old.evidence)
      for (const k of Object.keys(e)) if (/simultaneous/.test(k)) delete e[k];
    const report = parseTestReport(old);
    expect(report.evidence.every((e) => e.simultaneous === undefined)).toBe(true);
    const out = renderReport(report);
    expect(out).not.toMatch(/simultaneous|family-wise|all rules of this batch at once/i);
    expect(out).toContain(supported.bound_statement);
  });

  it('reads a v1 report and a state report without crashing, and shows nothing simultaneous', () => {
    for (const name of ['test_v1_real', 'test_state_p0']) {
      const out = renderReport(parseTestReport(fx(name)));
      expect(out).not.toMatch(/simultaneous|family-wise/i);
    }
  });

  it('refuses a half-present simultaneous bound rather than guessing', () => {
    const half = structuredClone(sim);
    delete half.evidence[0].upper_bound_simultaneous_confidence;
    expect(() => parseTestReport(half)).toThrow(/incomplete/);
  });

  it('refuses an out-of-range simultaneous bound', () => {
    const bad = structuredClone(sim);
    bad.evidence[0].breach_rate_upper_bound_simultaneous = 1.5;
    expect(() => parseTestReport(bad)).toThrow();
  });

  it('lets no unknown evidence key through, whatever its name', () => {
    const MARK = 'LEAK-7d1f';
    const hostile = structuredClone(sim);
    for (const e of hostile.evidence)
      for (const k of ['rows', 'example', 'values', 'simultaneous_rows', 'simultaneous_example'])
        e[k] = { [k]: MARK };
    const report = parseTestReport(hostile);
    const BOUND = ['confidence', 'method', 'observed', 'statement', 'upper'];
    const KEYS = [
      'bound',
      'confirmation',
      'decision',
      'discovery',
      'pValue',
      'rowsRequired',
      'ruleSha256',
      'simultaneous',
      'text',
    ];
    for (const e of report.evidence) {
      for (const key of Object.keys(e)) expect(KEYS).toContain(key);
      for (const key of Object.keys(e.bound!)) expect(BOUND).toContain(key);
      for (const key of Object.keys(e.simultaneous!))
        expect(['confidence', 'method', 'statement', 'upper']).toContain(key);
    }
    expect(JSON.stringify(report)).not.toContain(MARK);
    expect(renderReport(report)).not.toContain(MARK);
  });
});
