import { describe, expect, it, vi } from 'vitest';
import { EvolveCallError } from '../../src/evolve/rulesClient';
import {
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
    askSourceId: vi.fn(async () => 'src-1'),
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
        sourceId: 'src-1',
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
