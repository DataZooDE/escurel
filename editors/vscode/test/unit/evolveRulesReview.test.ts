/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it, vi } from 'vitest';
import { EvolveCallError } from '../../src/evolve/rulesClient';
import { readStateInstance } from '../../src/evolve/stateInstance';
import {
  confirmTestDetail,
  ruleChoices,
  reviewRules,
  startWithRules,
  type RulesApi,
  type RulesUi,
} from '../../src/evolve/rulesReview';
import {
  parseApproval,
  parseProposal,
  parseTestReport,
  type TestReport,
} from '../../src/evolve/rulesModel';

const H = (c: string) => c.repeat(64);
const SRC_INPUT = { kind: 'training_source', sourceId: 'src-1' } as const;
const rule = (sha: string, column: string) => ({
  rule_sha256: sha,
  rule: { kind: 'not_null', table: 't', column },
});
const proposal = parseProposal({
  batch_id: H('1'),
  data_sha256: H('2'),
  split: { day_cut: 60, days: 90 },
  candidates: [rule(H('a'), 'c1'), rule(H('b'), 'c2'), rule(H('c'), 'c3')],
});
const ev = (sha: string, column: string, decision: string, rows = 400) => ({
  rule: { kind: 'not_null', table: 't', column },
  rule_sha256: sha,
  discovery: { rows: 100, breaches: 0 },
  confirmation: { rows, breaches: 0 },
  p_value: 0.001,
  rows_required: 299,
  decision,
});
const report = (
  decisions: [string, string, string] = ['supported', 'supported', 'rejected'],
): TestReport =>
  parseTestReport({
    batch_id: H('1'),
    test_sha256: H('4'),
    policy: { p_max: 0.01, alpha: 0.05 },
    candidates_before: 0,
    supported: decisions.filter((d) => d === 'supported').length,
    insufficient_evidence: decisions.filter((d) => d === 'insufficient_evidence').length,
    rejected: decisions.filter((d) => d === 'rejected').length,
    evidence: [
      ev(H('a'), 'c1', decisions[0]),
      ev(H('b'), 'c2', decisions[1]),
      ev(H('c'), 'c3', decisions[2]),
    ],
  });
const approval = parseApproval({
  approved_rule_set_sha256: H('9'),
  rules: [rule(H('a'), 'c1')],
  data_sha256: H('2'),
  approver: 'alice',
});

function setup(
  over: {
    ui?: Partial<RulesUi>;
    api?: Partial<RulesApi>;
    subject?: () => Promise<string | undefined>;
  } = {},
) {
  const calls: string[] = [];
  const ui: RulesUi = {
    askInput: vi.fn(async () => SRC_INPUT),
    askPolicy: vi.fn(async () => ({})),
    confirmTest: vi.fn(async () => true),
    showReport: vi.fn(async () => undefined),
    pickRules: vi.fn(async () => [H('a')]),
    confirmApproval: vi.fn(async () => true),
    notify: vi.fn(),
    ...over.ui,
  };
  const api: RulesApi = {
    propose: vi.fn(async () => (calls.push('propose'), proposal)),
    test: vi.fn(async () => (calls.push('test'), report())),
    get: vi.fn(async () => (calls.push('get'), { test: report() })),
    approve: vi.fn(async () => (calls.push('approve'), approval)),
    ...over.api,
  };
  const subject = over.subject ?? (async () => 'alice');
  return { ui, api, calls, subject };
}

describe('review and approve discovered rules', () => {
  it('runs propose, test, review, explicit choice, explicit confirmation, then approve', async () => {
    const s = setup();
    const out = await reviewRules(s);
    expect(out.state).toBe('approved');
    expect(s.calls).toEqual(['propose', 'test', 'get', 'approve']);
    expect(s.api.approve).toHaveBeenCalledWith({
      batchId: H('1'),
      ruleSha256s: [H('a')],
      expectedTestSha256: H('4'),
    });
    // The report was shown before anything could be chosen.
    const show = (s.ui.showReport as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!;
    const pick = (s.ui.pickRules as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!;
    expect(show).toBeLessThan(pick);
  });

  it('never approves without the confirmation, whatever else happened', async () => {
    for (const ui of [
      { confirmApproval: vi.fn(async () => false) },
      { pickRules: vi.fn(async () => undefined) },
      { pickRules: vi.fn(async () => []) },
    ]) {
      const s = setup({ ui });
      const out = await reviewRules(s);
      expect(out.state).toBe('cancelled');
      expect(s.api.approve).not.toHaveBeenCalled();
    }
  });

  it('does not test a batch the person did not agree to test', async () => {
    const s = setup({ ui: { confirmTest: vi.fn(async () => false) } });
    expect((await reviewRules(s)).state).toBe('cancelled');
    expect(s.api.test).not.toHaveBeenCalled();
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('with only insufficient evidence it shows the report and offers nothing to approve', async () => {
    const s = setup({
      api: {
        test: vi.fn(async () =>
          report(['insufficient_evidence', 'insufficient_evidence', 'rejected']),
        ),
      },
    });
    const out = await reviewRules(s);
    expect(out.state).toBe('nothing_to_approve');
    expect(s.ui.showReport).toHaveBeenCalledOnce();
    expect(s.ui.pickRules).not.toHaveBeenCalled();
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('offers only the supported rules for selection', async () => {
    const s = setup();
    await reviewRules(s);
    const offered = (s.ui.pickRules as ReturnType<typeof vi.fn>).mock.calls[0]![0] as {
      ruleSha256: string;
    }[];
    expect(offered.map((e) => e.ruleSha256)).toEqual([H('a'), H('b')]);
  });

  it('refuses a chosen rule that was not offered', async () => {
    const s = setup({ ui: { pickRules: vi.fn(async () => [H('c')]) } });
    await expect(reviewRules(s)).rejects.toThrow(/not one of the supported rules/);
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('stops when the signed-in owner changes during the review', async () => {
    let calls = 0;
    const s = setup({ subject: async () => (++calls <= 1 ? 'alice' : 'mallory') });
    await expect(reviewRules(s)).rejects.toThrow(/signed-in owner changed/);
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('refuses to start without a signed-in owner', async () => {
    const s = setup({ subject: async () => undefined });
    await expect(reviewRules(s)).rejects.toThrow(/Sign in/);
    expect(s.api.propose).not.toHaveBeenCalled();
  });

  it('stops when the stored test is no longer the reviewed one (stale state)', async () => {
    const other = parseTestReport({
      ...{
        batch_id: H('1'),
        test_sha256: H('5'),
        policy: { p_max: 0.01, alpha: 0.05 },
        candidates_before: 0,
        supported: 0,
        insufficient_evidence: 0,
        rejected: 0,
        evidence: [],
      },
    });
    const s = setup({ api: { get: vi.fn(async () => ({ test: other })) } });
    await expect(reviewRules(s)).rejects.toMatchObject({ kind: 'stale' });
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('stops when the test answers for another batch or for rules that were never proposed', async () => {
    const wrongBatch = parseTestReport({
      batch_id: H('7'),
      test_sha256: H('4'),
      policy: { p_max: 0.01, alpha: 0.05 },
      candidates_before: 0,
      supported: 0,
      insufficient_evidence: 0,
      rejected: 0,
      evidence: [],
    });
    let s = setup({ api: { test: vi.fn(async () => wrongBatch) } });
    await expect(reviewRules(s)).rejects.toMatchObject({ kind: 'stale' });
    const foreign = parseTestReport({
      batch_id: H('1'),
      test_sha256: H('4'),
      policy: { p_max: 0.01, alpha: 0.05 },
      candidates_before: 0,
      supported: 1,
      insufficient_evidence: 0,
      rejected: 0,
      evidence: [ev(H('e'), 'zz', 'supported')],
    });
    s = setup({ api: { test: vi.fn(async () => foreign) } });
    await expect(reviewRules(s)).rejects.toMatchObject({ kind: 'stale' });
  });

  it('a service that is down surfaces its message and approves nothing', async () => {
    const s = setup({
      api: {
        propose: vi.fn(async () => {
          throw new EvolveCallError('unreachable', 'Evolve is not reachable');
        }),
      },
    });
    await expect(reviewRules(s)).rejects.toThrow(/not reachable/);
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('shows the confirmation the exact rules, test hash and what approval does not mean', async () => {
    const s = setup();
    await reviewRules(s);
    const summary = (s.ui.confirmApproval as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(summary).toContain(H('4'));
    expect(summary).toContain('t.c1 is never empty');
    expect(summary).not.toContain('t.c2');
    expect(summary).toMatch(/new experiments only/);
  });
});

describe('start a new experiment with the approved rules', () => {
  const spec = {
    pilot: 'p1_decision',
    experiment: 'exp-9',
    training_source_id: 'src-1',
    source_sha256: H('2'),
  };
  const startDeps = (over: { spec?: Record<string, unknown>; confirm?: boolean } = {}) => {
    const start = vi.fn(async () => ({ experiment: 'exp-9', state: 'queued' }));
    const confirm = vi.fn(async () => over.confirm ?? true);
    return {
      start,
      confirm,
      deps: {
        approval,
        input: SRC_INPUT,
        readSpec: async () => over.spec ?? spec,
        confirmStart: confirm,
        start,
      },
    };
  };

  it('attaches the approved hash after an explicit confirmation', async () => {
    const d = startDeps();
    const out = await startWithRules(d.deps);
    expect(out?.experiment).toBe('exp-9');
    expect(d.start).toHaveBeenCalledWith(spec, H('9'));
  });

  it('does nothing without the confirmation', async () => {
    const d = startDeps({ confirm: false });
    expect(await startWithRules(d.deps)).toBeUndefined();
    expect(d.start).not.toHaveBeenCalled();
  });

  it('refuses a spec for another source or other data than the rules were approved on', async () => {
    let d = startDeps({ spec: { ...spec, training_source_id: 'other' } });
    await expect(startWithRules(d.deps)).rejects.toThrow(/different training source/);
    d = startDeps({ spec: { ...spec, source_sha256: H('8') } });
    await expect(startWithRules(d.deps)).rejects.toThrow(/different data/);
    expect(d.start).not.toHaveBeenCalled();
  });

  it('needs an experiment id of its own', async () => {
    const noId: Record<string, unknown> = { ...spec };
    delete noId.experiment;
    const d = startDeps({ spec: noId });
    await expect(startWithRules(d.deps)).rejects.toThrow(/experiment id/);
  });
});

const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));

/** The real service output (policy v2, p_max 5%) as the api would hand it over. */
function realSetup(over: Parameters<typeof setup>[0] = {}, testName = 'test_v2_pmax5') {
  const raw = fx(testName);
  // The proposal of the same batch: the real candidate list for the pmax5 batch, and the rules the
  // real report names for the other (its proposal was not captured separately).
  const real = parseProposal(
    testName === 'test_v2_pmax5'
      ? fx('propose_v2')
      : {
          batch_id: raw.batch_id,
          data_sha256: H('2'),
          split: raw.split,
          candidates: raw.evidence.map((e: any) => ({ rule_sha256: e.rule_sha256, rule: e.rule })),
        },
  );
  const tested = parseTestReport(raw);
  const calls: string[] = [];
  const firstSupported = tested.evidence.find((e) => e.decision === 'supported')!.ruleSha256;
  const base = setup({
    ...over,
    ui: { pickRules: vi.fn(async () => [firstSupported]), ...over.ui },
    api: {
      propose: vi.fn(async () => real),
      test: vi.fn(async () => tested),
      get: vi.fn(async () => ({ test: tested })),
      approve: vi.fn(async () => approval),
      ...over.api,
    },
  });
  return { ...base, calls, raw, tested, real };
}

describe('choosing p_max and alpha before the one-shot test', () => {
  it('asks for the policy after the proposal and before the confirmation, and tests with the choice', async () => {
    const choice = { pMax: 0.05, alpha: 0.05 };
    const s = realSetup({ ui: { askPolicy: vi.fn(async () => choice) } });
    await reviewRules(s);
    const order = (f: unknown) => (f as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!;
    expect(order(s.api.propose)).toBeLessThan(order(s.ui.askPolicy));
    expect(order(s.ui.askPolicy)).toBeLessThan(order(s.ui.confirmTest));
    expect(order(s.ui.confirmTest)).toBeLessThan(order(s.api.test));
    expect(s.ui.confirmTest).toHaveBeenCalledWith(s.real, choice);
    expect(s.api.test).toHaveBeenCalledWith(s.real.batchId, choice);
  });

  it('keeps the service default when nothing is chosen', async () => {
    const s = realSetup();
    await reviewRules(s);
    expect(s.api.test).toHaveBeenCalledWith(s.real.batchId, {});
  });

  it('tests nothing if the person cancels the policy question', async () => {
    const s = realSetup({ ui: { askPolicy: vi.fn(async () => undefined) } });
    expect((await reviewRules(s)).state).toBe('cancelled');
    expect(s.ui.confirmTest).not.toHaveBeenCalled();
    expect(s.api.test).not.toHaveBeenCalled();
  });

  it('does not offer a choice for a state batch (the service takes none)', async () => {
    const stateProposal = parseProposal(fx('propose_state_p0'));
    const stateReport = parseTestReport(fx('test_state_p0'));
    const s = setup({
      ui: {
        askInput: vi.fn(async () => ({
          kind: 'instance' as const,
          instance: { pilot: 'p0', capacity: 10, items_csv: '6,5' },
        })),
        pickRules: vi.fn(async (o: { ruleSha256: string }[]) => [o[0]!.ruleSha256]),
      },
      api: {
        propose: vi.fn(async () => stateProposal),
        test: vi.fn(async () => stateReport),
        get: vi.fn(async () => ({ test: stateReport })),
      },
    });
    await reviewRules(s);
    expect(s.ui.askPolicy).not.toHaveBeenCalled();
    expect(s.api.test).toHaveBeenCalledWith(stateProposal.batchId, {});
    const shown = (s.ui.showReport as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(shown).toMatch(/replay evidence/i);
  });

  it('refuses a report that was made under a different policy than the one chosen', async () => {
    // The person chose 1%; the service (or something in between) answers with a 5% test.
    const s = realSetup({ ui: { askPolicy: vi.fn(async () => ({ pMax: 0.01 })) } });
    await expect(reviewRules(s)).rejects.toThrow(/different policy/);
    expect(s.ui.showReport).not.toHaveBeenCalled();
    expect(s.api.approve).not.toHaveBeenCalled();
  });

  it('says so when the batch had been tested before and the choice could not apply', async () => {
    const again = parseTestReport({ ...fx('test_v2_pmax5'), already_tested: true });
    const s = realSetup({
      ui: { askPolicy: vi.fn(async () => ({ pMax: 0.01, alpha: 0.05 })) },
      api: { test: vi.fn(async () => again) },
    });
    await reviewRules(s);
    const shown = (s.ui.showReport as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(shown).toMatch(/already tested[^\n]*not applied/);
    expect(s.ui.notify).toHaveBeenCalledWith(expect.stringMatching(/already tested/i));
  });
});

describe('approval stays bound to the exact reviewed test (real v2 report)', () => {
  it('sends exactly the reviewed hash, the chosen supported rules and nothing from the new fields', async () => {
    const s = realSetup({
      ui: {
        pickRules: vi.fn(async (offered: { ruleSha256: string }[]) => [offered[0]!.ruleSha256]),
      },
    });
    await reviewRules(s);
    const first = s.tested.evidence.find((e) => e.decision === 'supported')!;
    expect(s.api.approve).toHaveBeenCalledWith({
      batchId: s.real.batchId,
      ruleSha256s: [first.ruleSha256],
      expectedTestSha256: s.raw.test_sha256,
    });
  });

  it('offers only supported rules, pre-selects none, and never offers a low-bound insufficient one', async () => {
    const s = realSetup({}, 'test_v2_default_short');
    await reviewRules(s);
    const offered = (s.ui.pickRules as ReturnType<typeof vi.fn>).mock.calls[0]![0] as {
      decision: string;
      ruleSha256: string;
    }[];
    expect(offered.length).toBe(s.raw.supported);
    expect(offered.every((e) => e.decision === 'supported')).toBe(true);
    const lowInsufficient = s.raw.evidence.filter(
      (e: any) => e.decision === 'insufficient_evidence' && e.breach_rate_upper_bound < 0.01,
    );
    expect(lowInsufficient.length).toBeGreaterThan(0);
    for (const e of lowInsufficient)
      expect(offered.map((o) => o.ruleSha256)).not.toContain(e.rule_sha256);
  });

  it('refuses to approve a low-bound insufficient rule even if the chooser returns it', async () => {
    const s = realSetup({}, 'test_v2_default_short');
    const low = s.raw.evidence.find(
      (e: any) => e.decision === 'insufficient_evidence' && e.breach_rate_upper_bound < 0.01,
    );
    const t = realSetup(
      { ui: { pickRules: vi.fn(async () => [low.rule_sha256]) } },
      'test_v2_default_short',
    );
    await expect(reviewRules(t)).rejects.toThrow(/not one of the supported rules/);
    expect(t.api.approve).not.toHaveBeenCalled();
  });

  it('shows the report (with the bounds) before anything can be chosen, and never approves on its own', async () => {
    const s = realSetup({ ui: { confirmApproval: vi.fn(async () => false) } });
    const out = await reviewRules(s);
    expect(out.state).toBe('cancelled');
    expect(s.api.approve).not.toHaveBeenCalled();
    const shown = (s.ui.showReport as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(shown).toContain(s.raw.evidence[0].bound_statement);
    expect(shown).toContain(s.raw.test_sha256);
  });

  it('keeps the owner re-check and the stored-test check with the new report shape', async () => {
    let calls = 0;
    const s = realSetup({ subject: async () => (++calls <= 1 ? 'alice' : 'mallory') });
    await expect(reviewRules(s)).rejects.toThrow(/signed-in owner changed/);
    const other = parseTestReport({ ...fx('test_v2_pmax5'), test_sha256: 'e'.repeat(64) });
    const t = realSetup({ api: { get: vi.fn(async () => ({ test: other })) } });
    await expect(reviewRules(t)).rejects.toMatchObject({ kind: 'stale' });
    expect(t.api.approve).not.toHaveBeenCalled();
  });

  it('the confirmation names the informational bound as such and still shows the hash', async () => {
    const s = realSetup();
    await reviewRules(s);
    const summary = (s.ui.confirmApproval as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(summary).toContain(s.raw.test_sha256);
    expect(summary).toMatch(/informational/i);
    expect(summary).toMatch(/does not decide|not an approval signal/i);
  });
});

describe('the confirmation before the one-shot test', () => {
  const real = parseProposal(fx('propose_v2'));

  it('says before running that the test is one-shot and a wrong choice cannot be redone', () => {
    const text = confirmTestDetail(real, { pMax: 0.05, alpha: 0.05 });
    expect(text).toMatch(/one-shot per batch/);
    expect(text).toMatch(/cannot be redone/);
    expect(text).toMatch(/wrong p_max or alpha cannot be corrected/);
    expect(confirmTestDetail(real, {})).toMatch(/one-shot per batch/);
  });

  it('shows the choice, the default when nothing was chosen, and the caps', () => {
    expect(confirmTestDetail(real, { pMax: 0.05, alpha: 0.025 })).toContain(
      'p_max 5% (0.05), alpha 2.5% (0.025)',
    );
    expect(confirmTestDetail(real, {})).toContain(
      'p_max the service default, alpha the service default',
    );
    expect(confirmTestDetail(real, {})).toContain('p_max up to 5%, alpha up to 10%');
  });

  it('lists the real candidates and the split, and for a state batch says there is no p_max', () => {
    expect(confirmTestDetail(real, {})).toContain('is a multiple of 6');
    const state = confirmTestDetail(parseProposal(fx('propose_state_p0')), {});
    expect(state).toMatch(/replaying known programs/);
    expect(state).not.toMatch(/Test policy:/);
    expect(state).toMatch(/every p0_bin_assignment\.item_id exists in p0_items\.id/);
    expect(state).toMatch(/at least 16 rows/);
  });
});

describe('a state batch for one exact instance (P0, P5, P7)', () => {
  const instances: Record<string, Record<string, unknown>> = {
    p0: { pilot: 'p0', capacity: 10, items_csv: '6,5,4,7,2,3,8,1,9,4,6,2,5,3,7,1' },
    p5: fx('instance_p5'),
    p7: fx('instance_p7'),
  };
  const START = { experiment: 'exp-s', max_generations: 0 };

  function stateSetup(pilot: string, over: Parameters<typeof setup>[0] = {}) {
    const instance = readStateInstance({ ...instances[pilot], ...START });
    const proposed = parseProposal(fx(`propose_state_${pilot}`));
    const tested = parseTestReport(fx(`test_state_${pilot}`));
    const approved = parseApproval(fx(`approve_state_${pilot}`));
    const input = { kind: 'instance', instance } as const;
    const first = tested.evidence.find((e) => e.decision === 'supported')!.ruleSha256;
    const s = setup({
      ...over,
      ui: {
        askInput: vi.fn(async () => input),
        pickRules: vi.fn(async () => [first]),
        ...over.ui,
      },
      api: {
        propose: vi.fn(async () => proposed),
        test: vi.fn(async () => tested),
        get: vi.fn(async () => ({ test: tested })),
        approve: vi.fn(async () => approved),
        ...over.api,
      },
    });
    return { ...s, input, instance, proposed, tested, approved, first };
  }

  for (const pilot of ['p0', 'p5', 'p7']) {
    it(`${pilot}: proposes the instance, skips the p_max/alpha question, names the instance`, async () => {
      const s = stateSetup(pilot);
      const out = await reviewRules(s);
      expect(out.state).toBe('approved');
      expect(s.api.propose).toHaveBeenCalledWith(s.input);
      expect(s.ui.askPolicy).not.toHaveBeenCalled();
      expect(s.api.test).toHaveBeenCalledWith(s.proposed.batchId, {});
      const summary = (s.ui.confirmApproval as ReturnType<typeof vi.fn>).mock
        .calls[0]![0] as string;
      expect(summary).toContain(
        `for this exact instance (pilot ${pilot}, data hash prefix ${s.proposed.dataSha256.slice(0, 12)})`,
      );
      expect(summary).not.toMatch(/training source/);
      expect(summary).toContain(s.tested.testSha256);
      // the approval is bound to the reviewed test, for exactly the chosen rule
      expect(s.api.approve).toHaveBeenCalledWith({
        batchId: s.proposed.batchId,
        ruleSha256s: [s.first],
        expectedTestSha256: s.tested.testSha256,
      });
      // the outcome carries the instance, for the start
      expect(out.state === 'approved' && out.input).toEqual(s.input);
    });
  }

  it('never approves a state batch without the explicit choice and confirmation', async () => {
    for (const ui of [
      { confirmApproval: vi.fn(async () => false) },
      { pickRules: vi.fn(async () => undefined) },
      { pickRules: vi.fn(async () => []) },
    ]) {
      const s = stateSetup('p0', { ui });
      expect((await reviewRules(s)).state).toBe('cancelled');
      expect(s.api.approve).not.toHaveBeenCalled();
    }
    const s = stateSetup('p0', { ui: { confirmTest: vi.fn(async () => false) } });
    expect((await reviewRules(s)).state).toBe('cancelled');
    expect(s.api.test).not.toHaveBeenCalled();
  });

  it('a cancelled input stores nothing', async () => {
    const s = stateSetup('p0', { ui: { askInput: vi.fn(async () => undefined) } });
    expect((await reviewRules(s)).state).toBe('cancelled');
    expect(s.api.propose).not.toHaveBeenCalled();
  });

  it('the confirmation before the test names the instance, not a training source', () => {
    const proposed = parseProposal(fx('propose_state_p0'));
    const text = confirmTestDetail(proposed, {}, instances.p0);
    expect(text).toContain('this exact instance (pilot p0, data hash prefix 26eff570ba98)');
  });

  it('offers the supported rules with nothing pre-selected', () => {
    const tested = parseTestReport(fx('test_state_p0'));
    const items = ruleChoices(tested.evidence.filter((e) => e.decision === 'supported'));
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) expect(item).not.toHaveProperty('picked');
  });

  describe('start', () => {
    const deps = (pilot: string, specOver: Record<string, unknown> = {}, confirm = true) => {
      const s = stateSetup(pilot);
      const spec = { ...s.instance, ...specOver };
      const start = vi.fn(async (_spec: Record<string, unknown>, _hash: string) => ({
        experiment: 'exp-s',
        state: 'pending',
      }));
      const confirmStart = vi.fn(async (_summary: string) => confirm);
      return {
        s,
        spec,
        start,
        confirmStart,
        deps: {
          approval: s.approved,
          input: s.input,
          readSpec: async () => spec,
          confirmStart,
          start,
        },
      };
    };

    for (const pilot of ['p0', 'p5', 'p7'])
      it(`${pilot}: starts the same instance with the approved hash after one more confirmation`, async () => {
        const d = deps(pilot);
        const out = await startWithRules(d.deps);
        expect(out?.experiment).toBe('exp-s');
        expect(d.start).toHaveBeenCalledWith(d.spec, d.s.approved.ruleSetSha256);
        const summary = d.confirmStart.mock.calls[0]![0] as string;
        expect(summary).toMatch(/does not go through the Escurel plan review/);
        expect(summary).toContain(d.s.approved.ruleSetSha256);
        expect(summary).toMatch(/exact instance/);
        expect(summary).toMatch(/0 generation/);
      });

    it('does nothing without the confirmation', async () => {
      const d = deps('p0', {}, false);
      expect(await startWithRules(d.deps)).toBeUndefined();
      expect(d.start).not.toHaveBeenCalled();
    });

    it('refuses a start file for another pilot than the rules were approved for', async () => {
      const d = deps('p0');
      const other = { ...instances.p5, ...START };
      await expect(startWithRules({ ...d.deps, readSpec: async () => other })).rejects.toThrow(
        /different pilot/,
      );
      expect(d.start).not.toHaveBeenCalled();
    });

    it('refuses a start file whose instance differs from the one the rules were approved on', async () => {
      for (const change of [{ capacity: 12 }, { items_csv: '6,5,4' }]) {
        const d = deps('p0', change);
        await expect(startWithRules(d.deps)).rejects.toThrow(/different instance/);
        expect(d.start).not.toHaveBeenCalled();
      }
    });

    it('needs an experiment id and an explicit max_generations', async () => {
      const noId = deps('p0');
      const spec: Record<string, unknown> = { ...noId.spec };
      delete spec.experiment;
      await expect(startWithRules({ ...noId.deps, readSpec: async () => spec })).rejects.toThrow(
        /experiment id/,
      );
      const noGen = deps('p0');
      const spec2: Record<string, unknown> = { ...noGen.spec };
      delete spec2.max_generations;
      await expect(startWithRules({ ...noGen.deps, readSpec: async () => spec2 })).rejects.toThrow(
        /max_generations/,
      );
      expect(noGen.start).not.toHaveBeenCalled();
    });
  });
});

describe('the simultaneous bound changes nothing about what is approved', () => {
  it('shows both bounds in the report, and sends exactly the chosen rule and the reviewed test hash', async () => {
    const s = realSetup({}, 'test_v2_simultaneous');
    const out = await reviewRules(s);
    expect(out.state).toBe('approved');
    const shown = (s.ui.showReport as ReturnType<typeof vi.fn>).mock.calls[0]![0] as string;
    expect(shown).toContain(s.raw.evidence[1].simultaneous_bound_statement);
    expect(shown).toContain(s.raw.evidence[1].bound_statement);
    const supported = s.raw.evidence.find((e: any) => e.decision === 'supported');
    expect(s.api.approve).toHaveBeenCalledTimes(1);
    expect(s.api.approve).toHaveBeenCalledWith({
      batchId: s.raw.batch_id,
      ruleSha256s: [supported.rule_sha256],
      expectedTestSha256: s.raw.test_sha256,
    });
  });

  it('does not pre-select or offer a rule because of a low simultaneous bound', async () => {
    const s = realSetup({}, 'test_v2_simultaneous');
    await reviewRules(s);
    const offered = (s.ui.pickRules as ReturnType<typeof vi.fn>).mock.calls[0]![0] as {
      decision: string;
    }[];
    expect(offered.length).toBeGreaterThan(0);
    expect(offered.every((e) => e.decision === 'supported')).toBe(true);
    expect(offered).toHaveLength(s.raw.supported);
  });
});
