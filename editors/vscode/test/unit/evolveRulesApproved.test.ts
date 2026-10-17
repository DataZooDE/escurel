/* eslint-disable @typescript-eslint/no-explicit-any -- JSON fixtures captured from the real service */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EvolveShapeError, parseApproval, renderApproval } from '../../src/evolve/rulesModel';
import { EvolveCallError, getApprovedRules } from '../../src/evolve/rulesClient';
import { showApprovedRules, type ApprovedUi } from '../../src/evolve/approvedRules';
import type { TokenRefresher } from '../../src/auth/refresher';

/**
 * evolve_rules_get by rule-set hash, real output of the evolve-agent at origin/main aff2aaf (PR #46):
 * one approved set for a training source (p1_decision) and one for a P0 state instance. Provenance:
 * fixtures/evolve-rules/test_v1_real.provenance.txt. Only the hostile edits are made by hand.
 */
const fx = (name: string): any =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/evolve-rules', name + '.json'), 'utf8'));
const p1 = fx('get_approved_p1');
const p0 = fx('get_approved_state_p0');
const H = (c: string) => c.repeat(64);
const endpoint = 'http://127.0.0.1:8123';

describe('parseApproval on the evolve_rules_get shape', () => {
  it('reads the p1 set: rules from rule_summaries, approver, time, policy with caps, applies_to', () => {
    const a = parseApproval(p1);
    expect(a.ruleSetSha256).toBe(p1.approved_rule_set_sha256);
    expect(a.pilot).toBe('p1_decision');
    expect(a.approver).toBe('dev-user');
    expect(a.approvedAt).toBe(p1.approved_at);
    expect(a.appliesTo).toBe(p1.applies_to);
    expect(a.policy).toMatchObject({
      version: 'binom_holm_ub_v2',
      pMax: 0.05,
      alpha: 0.05,
      caps: { pMaxMax: 0.05, alphaMax: 0.1 },
    });
    expect(a.rules).toHaveLength(14);
    expect(a.rules[0]).toEqual({
      sha256: p1.rule_summaries[0].rule_sha256,
      text: 'demand.qty is never empty',
    });
  });

  it('reads the P0 state set: state_replay policy, aggregate and fk rules', () => {
    const a = parseApproval(p0);
    expect(a.pilot).toBe('p0');
    expect(a.policy).toEqual({ version: 'state_replay_v1' });
    expect(a.rules.map((r) => r.text)).toEqual([
      'p0_bin_assignment: each bin_id group has a total p0_items.size of at most 10 (rows joined to p0_items on item_id = id)',
      'every p0_bin_assignment.item_id exists in p0_items.id',
      'p0_bin_assignment: the whole table has at least 16 rows',
    ]);
  });

  it('tolerates an older service: no summaries, time, applies_to or caps (the approve receipt shape)', () => {
    const a = parseApproval(fx('approve_v2'));
    expect(a.approvedAt).toBeUndefined();
    expect(a.rules.length).toBeGreaterThan(0);
    const bare = parseApproval({
      approved_rule_set_sha256: H('9'),
      data_sha256: H('2'),
      approver: 'alice',
      rules: [{ rule_sha256: H('a'), rule: { kind: 'not_null', table: 't', column: 'c' } }],
    });
    expect(bare.approvedAt).toBeUndefined();
    expect(bare.appliesTo).toBeUndefined();
    expect(bare.policy).toBeUndefined();
  });

  it('drops a malformed time and refuses an approval with neither summaries nor rules', () => {
    expect(
      parseApproval({ ...p1, approved_at: 'yesterday-ish <script>' }).approvedAt,
    ).toBeUndefined();
    expect(() => parseApproval({ ...p1, rule_summaries: [], rules: [] })).toThrow(EvolveShapeError);
  });
});

describe('the approved-set document', () => {
  it('shows pilot, data hash prefix, approver, time, policy, caps, applies_to and each rule with its hash prefix', () => {
    const a = parseApproval(p1);
    const text = renderApproval(a);
    expect(text).toContain('p1_decision');
    expect(text).toContain(p1.approved_rule_set_sha256);
    expect(text).toContain(p1.data_sha256.slice(0, 12));
    expect(text).not.toContain(p1.data_sha256);
    expect(text).toContain('dev-user');
    expect(text).toContain('2026-10-17');
    expect(text).toContain('UTC');
    expect(text).toContain('binom_holm_ub_v2');
    expect(text).toMatch(/p_max 5%/);
    expect(text).toMatch(/alpha 5%/);
    expect(text).toMatch(/p_max may be up to 5%.*alpha up to 10%/);
    expect(text).toContain(p1.applies_to);
    for (const s of p1.rule_summaries) expect(text).toContain(s.rule_sha256.slice(0, 12));
    expect(text).toContain('demand.qty is a multiple of 6');
    expect(text).toContain('demand.qty is one of 0, 6, 12, 18, 24, 30, 36');
    expect(text).toContain('demand.qty is between 0 and 36');
    expect(text).toContain('demand.qty is never empty');
  });

  it('renders the P0 kinds (agg_le, fk_in, agg_ge) in words, not as JSON', () => {
    const text = renderApproval(parseApproval(p0));
    expect(text).toContain('state_replay_v1 (replay evidence, no statistical test)');
    expect(text).toContain('every p0_bin_assignment.item_id exists in p0_items.id');
    expect(text).toContain('the whole table has at least 16 rows');
    expect(text).toContain('a total p0_items.size of at most 10');
    expect(text).toContain('exactly this instance');
    expect(text).not.toMatch(/[{}]|"kind"|rule_summaries|"agg"/);
  });

  it('renders unique and an unknown kind without showing its fields', () => {
    const raw = structuredClone(p1);
    raw.rule_summaries = [
      { rule_sha256: H('a'), rule: { kind: 'unique', table: 't', columns: ['a', 'b'] } },
      { rule_sha256: H('b'), rule: { kind: 'telepathy', secret: 'SECRET-VALUE-77', rows: [1, 2] } },
    ];
    const text = renderApproval(parseApproval(raw));
    expect(text).toContain('t has no two rows with the same a, b');
    expect(text).toContain('rule kind "telepathy": details not rendered');
    expect(text).not.toContain('SECRET-VALUE-77');
  });

  it('shows only the whitelisted fields: hostile top-level and per-rule keys never reach the text', () => {
    const MARK = 'LEAK-3c9e';
    const raw = structuredClone(p1);
    for (const k of ['rows', 'example', 'values', 'row_values', 'token', 'notes']) raw[k] = MARK;
    for (const s of raw.rule_summaries) s.example = MARK;
    const a = parseApproval(raw);
    expect(JSON.stringify(a)).not.toContain(MARK);
    expect(renderApproval(a)).not.toContain(MARK);
    expect(Object.keys(a).sort()).toEqual(
      [
        'appliesTo',
        'approvedAt',
        'approver',
        'dataSha256',
        'pilot',
        'policy',
        'ruleSetSha256',
        'rules',
      ].sort(),
    );
  });

  it('cleans hostile text in approver and applies_to', () => {
    const text = renderApproval(
      parseApproval({
        ...p1,
        approver: 'al‮ice\u0000',
        applies_to: 'x\nFAKE LINE‮' + 'y'.repeat(2000),
      }),
    );
    expect(text).not.toContain('‮');
    expect(text).not.toContain('\nFAKE LINE');
    expect(text.length).toBeLessThan(8000);
  });

  it('says plainly when the set was approved before policies were recorded', () => {
    const text = renderApproval(parseApproval({ ...p1, policy: null }));
    expect(text).toMatch(/policy: not recorded/i);
  });

  it('is value-free and says so', () => {
    expect(renderApproval(parseApproval(p1))).toMatch(/No row values/);
  });
});

function refresher(tokens: (string | undefined)[] = ['tok-1', 'tok-2']) {
  let i = 0;
  return {
    get: vi.fn(async () => tokens[Math.min(i, tokens.length - 1)]),
    invalidate: vi.fn(async () => tokens[Math.min(++i, tokens.length - 1)]),
  } as unknown as TokenRefresher;
}
const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
function stub(...responses: (Response | Error)[]) {
  const fetcher = vi.fn(async (_url: string, _init: RequestInit) => {
    const next = responses.shift() ?? new Error('unexpected extra request');
    if (next instanceof Error) throw next;
    return next;
  });
  vi.stubGlobal('fetch', fetcher);
  return fetcher;
}
afterEach(() => vi.unstubAllGlobals());

describe('getApprovedRules (the client)', () => {
  const HASH = p1.approved_rule_set_sha256 as string;

  it('sends exactly the rule-set hash with the signed-in bearer, no redirect', async () => {
    const f = stub(json(200, p1));
    const a = await getApprovedRules(endpoint, refresher(), HASH);
    expect(a.ruleSetSha256).toBe(HASH);
    const call = f.mock.calls[0]!;
    expect(call[0]).toBe('http://127.0.0.1:8123/');
    const headers = (call[1] as RequestInit).headers as Record<string, string>;
    expect(headers['X-Triton-Tool']).toBe('evolve_rules_get');
    expect(headers.authorization).toBe('Bearer tok-1');
    expect((call[1] as RequestInit).redirect).toBe('error');
    expect(JSON.parse(String((call[1] as RequestInit).body))).toEqual({ rule_set_sha256: HASH });
  });

  it('refuses a malformed hash before sending anything', async () => {
    const f = stub();
    await expect(getApprovedRules(endpoint, refresher(), 'abc')).rejects.toMatchObject({
      kind: 'invalid',
    });
    await expect(getApprovedRules(endpoint, refresher(), HASH + 'x')).rejects.toMatchObject({
      kind: 'invalid',
    });
    expect(f).not.toHaveBeenCalled();
  });

  it('refuses an endpoint that is not an HTTPS origin', async () => {
    const f = stub();
    await expect(getApprovedRules('http://evolve.example.com', refresher(), HASH)).rejects.toThrow(
      /HTTPS origin/,
    );
    expect(f).not.toHaveBeenCalled();
  });

  it('refreshes the token once on a 401, then asks to sign in again', async () => {
    const r = refresher();
    stub(json(401, {}), json(200, p1));
    expect((await getApprovedRules(endpoint, r, HASH)).approver).toBe('dev-user');
    expect(r.invalidate).toHaveBeenCalledTimes(1);
    stub(json(401, {}), json(401, {}));
    await expect(getApprovedRules(endpoint, refresher(), HASH)).rejects.toMatchObject({
      kind: 'unauthorized',
    });
  });

  it('maps 404 (another owner or unknown hash look the same) to not_found', async () => {
    stub(json(404, { error: 'approved rule set not found' }));
    await expect(getApprovedRules(endpoint, refresher(), HASH)).rejects.toMatchObject({
      kind: 'not_found',
    });
  });

  it('says plainly when Evolve is down', async () => {
    stub(new Error('connect ECONNREFUSED'), new Error('x'), new Error('x'));
    await expect(getApprovedRules(endpoint, refresher(), HASH)).rejects.toMatchObject({
      kind: 'unreachable',
    });
  });

  it('refuses an answer for another rule set than the one asked for', async () => {
    stub(json(200, { ...p1, approved_rule_set_sha256: H('7') }));
    await expect(getApprovedRules(endpoint, refresher(), HASH)).rejects.toMatchObject({
      kind: 'stale',
    });
  });

  it('rejects a malformed answer rather than showing it', async () => {
    stub(json(200, { approved_rule_set_sha256: HASH }));
    await expect(getApprovedRules(endpoint, refresher(), HASH)).rejects.toBeInstanceOf(
      EvolveShapeError,
    );
  });
});

describe('the Show approved rules flow', () => {
  const HASH = p1.approved_rule_set_sha256 as string;
  function setup(
    over: Partial<Parameters<typeof showApprovedRules>[0]> = {},
    ui: Partial<ApprovedUi> = {},
  ) {
    const shown: string[] = [];
    const fullUi: ApprovedUi = {
      pickHash: vi.fn(async () => HASH),
      show: vi.fn(async (t: string) => void shown.push(t)),
      ...ui,
    };
    const get = vi.fn(async () => parseApproval(p1));
    const deps = {
      ui: fullUi,
      get,
      subject: vi.fn(async () => 'alice' as string | undefined),
      remembered: vi.fn(() => [] as string[]),
      ...over,
    };
    return { deps, shown, get, ui: fullUi };
  }

  it('asks for a hash (offering the remembered ones), reads that set, and shows its document', async () => {
    const s = setup({ remembered: vi.fn(() => [HASH, H('5')]) });
    const out = await showApprovedRules(s.deps);
    expect(s.ui.pickHash).toHaveBeenCalledWith([HASH, H('5')]);
    expect(s.get).toHaveBeenCalledWith(HASH);
    expect(s.shown).toHaveLength(1);
    expect(s.shown[0]).toContain('demand.qty is never empty');
    expect(out?.ruleSetSha256).toBe(HASH);
  });

  it('remembered hashes are asked for the signed-in owner only', async () => {
    const remembered = vi.fn(() => []);
    await showApprovedRules(setup({ remembered }).deps);
    expect(remembered).toHaveBeenCalledWith('alice');
  });

  it('uses the given hash without asking (the approval toast)', async () => {
    const s = setup();
    await showApprovedRules(s.deps, HASH);
    expect(s.ui.pickHash).not.toHaveBeenCalled();
    expect(s.get).toHaveBeenCalledWith(HASH);
  });

  it('does nothing when the person cancels', async () => {
    const s = setup({}, { pickHash: vi.fn(async () => undefined) });
    expect(await showApprovedRules(s.deps)).toBeUndefined();
    expect(s.get).not.toHaveBeenCalled();
    expect(s.shown).toHaveLength(0);
  });

  it('normalises typed input and refuses what is not 64 hex before any request', async () => {
    const s = setup({}, { pickHash: vi.fn(async () => `  ${HASH.toUpperCase()}\n`) });
    await showApprovedRules(s.deps);
    expect(s.get).toHaveBeenCalledWith(HASH);
    const bad = setup({}, { pickHash: vi.fn(async () => 'not-a-hash') });
    await expect(showApprovedRules(bad.deps)).rejects.toMatchObject({ kind: 'invalid' });
    expect(bad.get).not.toHaveBeenCalled();
  });

  it('requires a signed-in owner before asking or reading anything', async () => {
    const s = setup({ subject: vi.fn(async () => undefined) });
    await expect(showApprovedRules(s.deps)).rejects.toThrow(/Sign in/);
    expect(s.ui.pickHash).not.toHaveBeenCalled();
    expect(s.get).not.toHaveBeenCalled();
  });

  it('shows nothing and names the hash when Evolve has no such set for this owner (404)', async () => {
    const s = setup({
      get: vi.fn(async () => {
        throw new EvolveCallError('not_found', 'Evolve has no such record for the signed-in user.');
      }),
    });
    const failure = (await showApprovedRules(s.deps).catch((e: unknown) => e)) as Error;
    expect(failure).toMatchObject({ kind: 'not_found' });
    expect(failure.message).toMatch(/not found|no approved rule set/i);
    expect(failure.message).toContain(HASH.slice(0, 12));
    expect(s.shown).toHaveLength(0);
  });

  it('shows nothing when the signed-in owner changed while the set was read', async () => {
    const subject = vi.fn<() => Promise<string | undefined>>();
    subject.mockResolvedValueOnce('alice').mockResolvedValueOnce('mallory');
    const s = setup({ subject });
    await expect(showApprovedRules(s.deps)).rejects.toThrow(/owner changed/);
    expect(s.shown).toHaveLength(0);
  });

  it('passes other failures (Evolve down, 401) through for the caller to report', async () => {
    const down = new EvolveCallError('unreachable', 'Evolve is not reachable at x.');
    const s = setup({ get: vi.fn(async () => Promise.reject(down)) });
    await expect(showApprovedRules(s.deps)).rejects.toBe(down);
    expect(s.shown).toHaveLength(0);
  });
});
