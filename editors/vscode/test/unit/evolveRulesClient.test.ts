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
    await expect(proposeRules('http://evolve.example.com', refresher(), 'src')).rejects.toThrow(
      /HTTPS origin/,
    );
    await expect(
      proposeRules('https://u:p@evolve.example.com', refresher(), 'src'),
    ).rejects.toThrow();
    await expect(proposeRules('', refresher(), 'src')).rejects.toThrow(/escurel.evolveEndpoint/);
    expect(f).not.toHaveBeenCalled();
  });

  it('refuses to call without a signed-in token', async () => {
    const f = stub();
    await expect(proposeRules(endpoint, refresher([undefined]), 'src')).rejects.toMatchObject({
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
    const failure = await proposeRules(endpoint, refresher(), 'src').catch((e: unknown) => e);
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
    await expect(proposeRules(endpoint, refresher(), 'src')).rejects.toMatchObject({
      kind: 'invalid',
    });
  });

  it('never echoes an unbounded or hostile server message', async () => {
    stub(json(422, { error: 'bad‮' + 'x'.repeat(5000) }));
    const failure = (await proposeRules(endpoint, refresher(), 'src').catch(
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
    await expect(
      startWithApprovedRules(endpoint, refresher(), { ...spec, pilot: 'p0' }, H('9')),
    ).rejects.toThrow(/p1_decision/);
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
