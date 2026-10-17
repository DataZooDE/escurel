/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  EvolveCallError,
  approveRules,
  proposeRules,
  startWithApprovedRules,
  testRules,
} from '../../src/evolve/rulesClient';
import type { TokenRefresher } from '../../src/auth/refresher';

const H = (c: string) => c.repeat(64);
const endpoint = 'http://127.0.0.1:8123';
const SRC = { kind: 'training_source', sourceId: 'src' } as const;

function refresher(tokens: (string | undefined)[] = ['tok-1', 'tok-2']) {
  let i = 0;
  return {
    get: vi.fn(async () => tokens[Math.min(i, tokens.length - 1)]),
    invalidate: vi.fn(async () => tokens[Math.min(++i, tokens.length - 1)]),
  } as unknown as TokenRefresher;
}

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

const evidence = {
  rule: { kind: 'not_null', table: 't', column: 'c' },
  rule_sha256: H('a'),
  discovery: { rows: 10, breaches: 0 },
  confirmation: { rows: 400, breaches: 0 },
  p_value: 0.001,
  rows_required: 299,
  decision: 'supported',
};
const testBody = {
  batch_id: H('1'),
  test_sha256: H('4'),
  policy: { p_max: 0.01, alpha: 0.05 },
  candidates_before: 0,
  supported: 1,
  insufficient_evidence: 0,
  rejected: 0,
  evidence: [evidence],
};

afterEach(() => vi.unstubAllGlobals());

function stub(...responses: (Response | Error)[]) {
  const fetcher = vi.fn(async (_url: string, _init: RequestInit) => {
    const next = responses.shift() ?? new Error('unexpected extra request');
    if (next instanceof Error) throw next;
    return next;
  });
  vi.stubGlobal('fetch', fetcher);
  return fetcher;
}

const header = (call: unknown[], name: string) =>
  ((call[1] as RequestInit).headers as Record<string, string>)[name];

describe('Evolve rule tools from the extension', () => {
  it('names the tool, sends the signed-in bearer, never follows a redirect', async () => {
    const f = stub(json(200, testBody));
    const report = await testRules(endpoint, refresher(), H('1'));
    expect(report.testSha256).toBe(H('4'));
    const call = f.mock.calls[0]!;
    expect(call[0]).toBe('http://127.0.0.1:8123/');
    expect(header(call, 'X-Triton-Tool')).toBe('evolve_rules_test');
    expect(header(call, 'authorization')).toBe('Bearer tok-1');
    expect((call[1] as RequestInit).redirect).toBe('error');
    expect(JSON.parse(String((call[1] as RequestInit).body))).toEqual({ batch_id: H('1') });
  });

  it('refuses an endpoint that is not an HTTPS origin before sending anything', async () => {
    const f = stub();
    await expect(proposeRules('http://evolve.example.com', refresher(), SRC)).rejects.toThrow(
      /HTTPS origin/,
    );
    await expect(
      proposeRules('https://u:p@evolve.example.com', refresher(), SRC),
    ).rejects.toThrow();
    await expect(proposeRules('', refresher(), SRC)).rejects.toThrow(/escurel.evolveEndpoint/);
    expect(f).not.toHaveBeenCalled();
  });

  it('refuses to call without a signed-in token', async () => {
    const f = stub();
    await expect(proposeRules(endpoint, refresher([undefined]), SRC)).rejects.toMatchObject({
      kind: 'signed_out',
    });
    expect(f).not.toHaveBeenCalled();
  });

  it('refreshes the token once on a 401 and then tells the user to sign in again', async () => {
    const ok = stub(json(401, {}), json(200, testBody));
    const r = refresher();
    await testRules(endpoint, r, H('1'));
    expect(r.invalidate).toHaveBeenCalledOnce();
    expect(header(ok.mock.calls[1]!, 'authorization')).toBe('Bearer tok-2');

    stub(json(401, {}), json(401, {}));
    await expect(testRules(endpoint, refresher(), H('1'))).rejects.toMatchObject({
      kind: 'unauthorized',
    });
  });

  it('says plainly when Evolve is down, after retrying a safe call', async () => {
    const f = stub(
      new TypeError('fetch failed'),
      new TypeError('fetch failed'),
      new TypeError('x'),
    );
    const failure = await proposeRules(endpoint, refresher(), SRC).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(EvolveCallError);
    expect((failure as EvolveCallError).kind).toBe('unreachable');
    expect((failure as EvolveCallError).message).toMatch(/not reachable/i);
    expect(f).toHaveBeenCalledTimes(3);
  });

  it('retries a 503 and then reports Evolve as busy', async () => {
    stub(json(503, {}), json(200, testBody));
    expect((await testRules(endpoint, refresher(), H('1'))).supported).toBe(1);
    stub(json(503, {}), json(503, {}), json(503, {}));
    await expect(testRules(endpoint, refresher(), H('1'))).rejects.toMatchObject({ kind: 'busy' });
  });

  it('maps 403, 404, 409 and 422 to messages a person can act on', async () => {
    stub(
      json(403, { error: 'rule approval is disabled: EVOLVE_RULE_APPROVER_PRINCIPALS is not set' }),
    );
    await expect(
      approveRules(endpoint, refresher(), {
        batchId: H('1'),
        ruleSha256s: [H('a')],
        expectedTestSha256: H('4'),
      }),
    ).rejects.toMatchObject({ kind: 'forbidden' });
    stub(json(404, { error: 'batch not found' }));
    await expect(testRules(endpoint, refresher(), H('1'))).rejects.toMatchObject({
      kind: 'not_found',
    });
    stub(json(409, { error: 'the training source changed since the batch' }));
    const stale = await testRules(endpoint, refresher(), H('1')).catch((e: unknown) => e);
    expect(stale).toMatchObject({ kind: 'stale' });
    expect((stale as Error).message).toMatch(/changed/);
    stub(json(422, { error: 'training source not found' }));
    await expect(proposeRules(endpoint, refresher(), SRC)).rejects.toMatchObject({
      kind: 'invalid',
    });
  });

  it('never echoes an unbounded or hostile server message', async () => {
    stub(json(422, { error: 'bad‮' + 'x'.repeat(5000) }));
    const failure = (await proposeRules(endpoint, refresher(), SRC).catch(
      (e: unknown) => e,
    )) as Error;
    expect(failure.message.length).toBeLessThan(500);
    expect(failure.message).not.toContain('‮');
  });

  it('rejects a malformed answer rather than showing it', async () => {
    stub(json(200, { ...testBody, test_sha256: 'x' }));
    await expect(testRules(endpoint, refresher(), H('1'))).rejects.toThrow(/invalid test hash/);
  });

  it('approves only with confirm:true, the reviewed test hash and hashes of rules', async () => {
    const f = stub(
      json(200, {
        approved_rule_set_sha256: H('9'),
        rules: [{ rule_sha256: H('a'), rule: evidence.rule }],
        data_sha256: H('2'),
        approver: 'alice',
      }),
    );
    const approval = await approveRules(endpoint, refresher(), {
      batchId: H('1'),
      ruleSha256s: [H('a')],
      expectedTestSha256: H('4'),
    });
    expect(approval.ruleSetSha256).toBe(H('9'));
    const call = f.mock.calls[0]!;
    expect(header(call, 'X-Triton-Tool')).toBe('evolve_rules_approve');
    expect(JSON.parse(String((call[1] as RequestInit).body))).toEqual({
      batch_id: H('1'),
      rule_sha256s: [H('a')],
      expected_test_sha256: H('4'),
      confirm: true,
    });
  });

  it('refuses an empty or malformed approval before any request', async () => {
    const f = stub();
    for (const bad of [
      { batchId: H('1'), ruleSha256s: [], expectedTestSha256: H('4') },
      { batchId: H('1'), ruleSha256s: ['nope'], expectedTestSha256: H('4') },
      { batchId: H('1'), ruleSha256s: [H('a'), H('a')], expectedTestSha256: H('4') },
      { batchId: H('1'), ruleSha256s: [H('a')], expectedTestSha256: 'x' },
      { batchId: 'x', ruleSha256s: [H('a')], expectedTestSha256: H('4') },
    ])
      await expect(approveRules(endpoint, refresher(), bad)).rejects.toThrow();
    expect(f).not.toHaveBeenCalled();
  });

  it('starts a NEW replenishment experiment with the approved hash and nothing server-owned', async () => {
    const f = stub(json(202, { experiment: 'exp-1', state: 'queued' }));
    const spec = { pilot: 'p1_decision', experiment: 'exp-1', training_source_id: 's' };
    await startWithApprovedRules(endpoint, refresher(), spec, H('9'));
    const call = f.mock.calls[0]!;
    expect(header(call, 'X-Triton-Tool')).toBe('evolve_start');
    expect(JSON.parse(String((call[1] as RequestInit).body))).toEqual({
      ...spec,
      approved_rule_set_sha256: H('9'),
    });
    // A start spec that already carries rules, or a different pilot, is refused locally.
    await expect(
      startWithApprovedRules(endpoint, refresher(), { ...spec, rule_set: [] }, H('9')),
    ).rejects.toThrow(/set by the server/);
    // Other pilots are refused: a state pilot needs its own instance keys, a P1 file is not one.
    for (const pilot of ['p2', 'p3', 'p8', 'p1', undefined])
      await expect(
        startWithApprovedRules(endpoint, refresher(), { ...spec, pilot }, H('9')),
      ).rejects.toThrow(/p1_decision|p0, p5 or p7/);
    await expect(
      startWithApprovedRules(endpoint, refresher(), { ...spec, pilot: 'p0' }, H('9')),
    ).rejects.toThrow(/training_source_id|capacity|items_csv/);
    await expect(
      startWithApprovedRules(
        endpoint,
        refresher(),
        { ...spec, approved_rule_set_sha256: H('8') },
        H('9'),
      ),
    ).rejects.toThrow(/already names/);
    expect(f).toHaveBeenCalledOnce();
  });

  it('does not retry a start after a lost response: a start is not retried blindly', async () => {
    const f = stub(new TypeError('fetch failed'));
    await expect(
      startWithApprovedRules(
        endpoint,
        refresher(),
        { pilot: 'p1_decision', experiment: 'e' },
        H('9'),
      ),
    ).rejects.toMatchObject({ kind: 'unreachable' });
    expect(f).toHaveBeenCalledOnce();
  });
});

const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));
const sent = (f: ReturnType<typeof stub>) =>
  JSON.parse(String((f.mock.calls[0]![1] as RequestInit).body));

describe('choosing p_max and alpha at test time', () => {
  it('sends only what was chosen, and nothing when nothing was', async () => {
    let f = stub(json(200, fx('test_v2_pmax5')));
    await testRules(endpoint, refresher(), H('1'), {});
    expect(sent(f)).toEqual({ batch_id: H('1') });
    f = stub(json(200, fx('test_v2_pmax5')));
    await testRules(endpoint, refresher(), H('1'), { pMax: 0.05 });
    expect(sent(f)).toEqual({ batch_id: H('1'), p_max: 0.05 });
    f = stub(json(200, fx('test_v2_pmax5')));
    const report = await testRules(endpoint, refresher(), H('1'), { pMax: 0.05, alpha: 0.05 });
    expect(sent(f)).toEqual({ batch_id: H('1'), p_max: 0.05, alpha: 0.05 });
    expect(report.policy.version).toBe('binom_holm_ub_v2');
    expect(report.evidence[0]!.bound?.statement).toMatch(/held-out rows/);
  });

  it('enforces the caps before sending anything', async () => {
    const f = stub(json(200, fx('test_v2_pmax5')));
    for (const bad of [
      { pMax: 0.0500001 },
      { pMax: 0.2 },
      { pMax: 0 },
      { pMax: -0.01 },
      { pMax: Number.NaN },
      { pMax: Number.POSITIVE_INFINITY },
      { alpha: 0.11 },
      { alpha: 0 },
      { alpha: Number.NaN },
    ])
      await expect(testRules(endpoint, refresher(), H('1'), bad)).rejects.toMatchObject({
        kind: 'invalid',
      });
    expect(f).not.toHaveBeenCalled();
  });

  it('accepts the caps themselves', async () => {
    const f = stub(json(200, fx('test_v2_pmax5')));
    await testRules(endpoint, refresher(), H('1'), { pMax: 0.05, alpha: 0.1 });
    expect(sent(f)).toEqual({ batch_id: H('1'), p_max: 0.05, alpha: 0.1 });
  });

  it('shows the service refusal when the service enforces a cap itself (real 422 body)', async () => {
    const real = fx('test_rejected_pmax');
    stub(json(real.status, real.body));
    await expect(testRules(endpoint, refresher(), H('1'), { pMax: 0.04 })).rejects.toMatchObject({
      kind: 'invalid',
      message: expect.stringContaining('p_max must be in (0, 0.05]'),
    });
  });

  it('reads a state_replay_v1 report from a state batch', async () => {
    stub(json(200, fx('test_state_p0')));
    const report = await testRules(endpoint, refresher(), H('1'));
    expect(report.kind).toBe('state_replay');
  });
});

describe('approval request is unchanged by the new report fields', () => {
  it('sends exactly batch, rules, the reviewed test hash and confirm', async () => {
    const f = stub(json(200, fx('approve_v2')));
    const out = await approveRules(endpoint, refresher(), {
      batchId: H('1'),
      ruleSha256s: [H('a')],
      expectedTestSha256: H('4'),
    });
    expect(sent(f)).toEqual({
      batch_id: H('1'),
      rule_sha256s: [H('a')],
      expected_test_sha256: H('4'),
      confirm: true,
    });
    expect(out.policy?.version).toBe('binom_holm_ub_v2');
  });
});

const STATE_START = { experiment: 'exp-s', max_generations: 0, budget: { max_evaluated: 1 } };
const instances: Record<string, Record<string, unknown>> = {
  p0: { pilot: 'p0', capacity: 10, items_csv: '6,5,4,7,2,3,8,1,9,4,6,2,5,3,7,1' },
  p5: fx('instance_p5'),
  p7: fx('instance_p7'),
};

describe('proposing rules for an exact instance (P0, P5, P7)', () => {
  for (const pilot of Object.keys(instances)) {
    it(`${pilot}: sends the instance and nothing of the start keys`, async () => {
      const f = stub(json(200, fx(pilot === 'p0' ? 'propose_state_p0' : `propose_state_${pilot}`)));
      const proposal = await proposeRules(endpoint, refresher(), {
        kind: 'instance',
        instance: { ...instances[pilot], ...STATE_START },
      });
      expect(header(f.mock.calls[0]!, 'X-Triton-Tool')).toBe('evolve_rules_propose');
      expect(sent(f)).toEqual(instances[pilot]);
      expect(proposal.kind).toBe('state');
    });
  }

  it('refuses an unknown key before anything is sent', async () => {
    const f = stub(json(200, fx('propose_state_p0')));
    for (const extra of ['synthetic_brain', 'substitution_csv', 'rule_set', 'training_source_id'])
      await expect(
        proposeRules(endpoint, refresher(), {
          kind: 'instance',
          instance: { ...instances.p0, [extra]: 'x' },
        }),
      ).rejects.toThrow(new RegExp(extra));
    expect(f).not.toHaveBeenCalled();
  });

  it('keeps the training-source request exactly as it was', async () => {
    const f = stub(json(200, fx('propose_v2')));
    await proposeRules(endpoint, refresher(), SRC);
    expect(sent(f)).toEqual({ training_source_id: 'src' });
  });
});

describe('starting a state experiment with approved rules', () => {
  for (const pilot of Object.keys(instances)) {
    it(`${pilot}: sends exactly the instance, the start keys, brain llm and the hash`, async () => {
      const f = stub(json(202, { experiment: 'exp-s', state: 'queued' }));
      await startWithApprovedRules(
        endpoint,
        refresher(),
        { ...instances[pilot], ...STATE_START },
        H('9'),
      );
      expect(header(f.mock.calls[0]!, 'X-Triton-Tool')).toBe('evolve_start');
      expect(sent(f)).toEqual({
        ...instances[pilot],
        ...STATE_START,
        brain: 'llm',
        approved_rule_set_sha256: H('9'),
      });
    });
  }

  it('refuses before sending: no max_generations, another brain, an unknown or server-owned key', async () => {
    const f = stub(json(202, {}));
    const base = { ...instances.p0, ...STATE_START };
    const noGen: Record<string, unknown> = { ...base };
    delete noGen.max_generations;
    for (const bad of [
      noGen,
      { ...base, brain: 'scripted' },
      { ...base, synthetic_brain: 'x' },
      { ...base, rule_set: [] },
      { ...base, approved_rule_set_sha256: H('8') },
      { ...base, rule_set_p_max: 0.05 },
    ])
      await expect(startWithApprovedRules(endpoint, refresher(), bad, H('9'))).rejects.toThrow();
    expect(f).not.toHaveBeenCalled();
  });

  it('reads the real receipt of a state start (dispatch_state, no state field)', async () => {
    stub(
      json(202, {
        accepted: true,
        dispatch_state: 'pending',
        experiment: 'exp-s',
        idempotent: false,
      }),
    );
    expect(
      await startWithApprovedRules(
        endpoint,
        refresher(),
        { ...instances.p0, ...STATE_START },
        H('9'),
      ),
    ).toEqual({ experiment: 'exp-s', state: 'pending' });
  });

  it('does not retry a lost state start', async () => {
    const f = stub(new TypeError('fetch failed'));
    await expect(
      startWithApprovedRules(endpoint, refresher(), { ...instances.p0, ...STATE_START }, H('9')),
    ).rejects.toMatchObject({ kind: 'unreachable' });
    expect(f).toHaveBeenCalledOnce();
  });
});
