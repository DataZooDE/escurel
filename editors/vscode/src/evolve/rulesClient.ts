import type { TokenRefresher } from '../auth/refresher';
import { cleanText } from '../shared/untrustedText';
import { evolveOrigin } from './holdoutClient';
import {
  ALPHA_CAP,
  P_MAX_CAP,
  isHash,
  parseApproval,
  parseProposal,
  parseTestReport,
  type Approval,
  type PolicyChoice,
  type Proposal,
  type TestReport,
} from './rulesModel';

export type EvolveCallErrorKind =
  | 'signed_out'
  | 'unauthorized'
  | 'unreachable'
  | 'busy'
  | 'forbidden'
  | 'not_found'
  | 'stale'
  | 'invalid'
  | 'server';

/** A failed call to Evolve, with a kind the caller can branch on and a message a person can read. */
export class EvolveCallError extends Error {
  constructor(
    readonly kind: EvolveCallErrorKind,
    message: string,
  ) {
    super(message);
  }
}

const FIXED: Record<number, [EvolveCallErrorKind, string]> = {
  403: ['forbidden', 'Evolve denied this action for the signed-in user.'],
  404: ['not_found', 'Evolve has no such record for the signed-in user.'],
  409: ['stale', 'The data or the reviewed test changed. Start the review again.'],
  422: ['invalid', 'Evolve rejected the request.'],
};

async function detail(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: unknown };
    return typeof body.error === 'string' ? ` ${cleanText(body.error, 240)}` : '';
  } catch {
    return '';
  }
}

interface CallOptions {
  /** Retry a lost request or a 503: only for calls Evolve answers idempotently. */
  retry: boolean;
  timeoutMs: number;
}

/**
 * One tool call to the Evolve service, as the signed-in user: the same endpoint validation, bearer
 * token and refresh-once-on-401 as the holdout registration. The owner is whoever the token names;
 * nothing here lets a caller pick one.
 */
export async function callEvolve(
  endpoint: string,
  refresher: TokenRefresher,
  tool: string,
  args: Record<string, unknown>,
  options: CallOptions,
): Promise<unknown> {
  const origin = evolveOrigin(endpoint);
  const body = JSON.stringify(args);
  const attempts = options.retry ? 3 : 1;
  let refreshed = false;
  let lastStatus = 0;
  for (let attempt = 0; attempt < attempts + 1; attempt++) {
    const token =
      refreshed && lastStatus === 401 ? await refresher.invalidate() : await refresher.get();
    if (!token)
      throw new EvolveCallError(
        'signed_out',
        'Sign in with an OIDC token accepted by Evolve first.',
      );
    let response: Response;
    try {
      response = await fetch(origin + '/', {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.timeout(options.timeoutMs),
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          'X-Triton-Tool': tool,
        },
        body,
      });
    } catch {
      if (attempt + 1 < attempts) continue;
      throw new EvolveCallError(
        'unreachable',
        `Evolve is not reachable at ${origin}. Check that the service is running and escurel.evolveEndpoint is right, then try again.`,
      );
    }
    lastStatus = response.status;
    if (response.status === 401) {
      if (!refreshed) {
        refreshed = true;
        continue;
      }
      throw new EvolveCallError(
        'unauthorized',
        'Evolve rejected the signed-in token. Check its OIDC audience and sign in again.',
      );
    }
    if (response.status === 503) {
      if (options.retry && attempt + 1 < attempts) continue;
      throw new EvolveCallError('busy', 'Evolve is busy or starting up. Try again in a moment.');
    }
    if (!response.ok) {
      const [kind, text] = FIXED[response.status] ?? [
        'server' as const,
        `Evolve failed (HTTP ${response.status}).`,
      ];
      throw new EvolveCallError(kind, text + (await detail(response)));
    }
    return response.json();
  }
  throw new EvolveCallError('server', 'Evolve did not complete the request.');
}

const safe: CallOptions = { retry: true, timeoutMs: 120_000 };

/** Evolve freezes a batch of candidate rules from the discovery window of the source. */
export async function proposeRules(
  endpoint: string,
  refresher: TokenRefresher,
  sourceId: string,
): Promise<Proposal> {
  return parseProposal(
    await callEvolve(
      endpoint,
      refresher,
      'evolve_rules_propose',
      { training_source_id: sourceId },
      safe,
    ),
  );
}

/**
 * The batch's one confirmation test. Asking again returns the stored report, whatever is asked now.
 * `choice` is what the person picked for p_max and alpha; a missing field is left out of the request
 * so the service default applies. The caps are checked here first and again by the service.
 */
export async function testRules(
  endpoint: string,
  refresher: TokenRefresher,
  batchId: string,
  choice: PolicyChoice = {},
): Promise<TestReport> {
  if (!isHash(batchId)) throw new EvolveCallError('invalid', 'A batch id is a 64-character hash.');
  const args: Record<string, unknown> = { batch_id: batchId };
  for (const [key, value, cap] of [
    ['p_max', choice.pMax, P_MAX_CAP],
    ['alpha', choice.alpha, ALPHA_CAP],
  ] as const) {
    if (value === undefined) continue;
    if (!Number.isFinite(value) || value <= 0 || value > cap)
      throw new EvolveCallError(
        'invalid',
        `${key} must be above 0 and at most ${cap} (the service's cap).`,
      );
    args[key] = value;
  }
  return parseTestReport(await callEvolve(endpoint, refresher, 'evolve_rules_test', args, safe));
}

/** Read a batch and its stored test (used to see whether a reviewed test is still the stored one). */
export async function getRuleBatch(
  endpoint: string,
  refresher: TokenRefresher,
  batchId: string,
): Promise<{ batchId: string; test: TestReport | undefined }> {
  if (!isHash(batchId)) throw new EvolveCallError('invalid', 'A batch id is a 64-character hash.');
  const raw = (await callEvolve(
    endpoint,
    refresher,
    'evolve_rules_get',
    { batch_id: batchId },
    safe,
  )) as {
    test?: unknown;
  };
  return { batchId, test: raw.test ? parseTestReport(raw.test) : undefined };
}

/**
 * Approve some supported rules. Only called after a person confirmed exactly these rules for exactly
 * this test; `confirm: true` is what Evolve requires of that decision, so it is sent here and nowhere
 * else, and never from a code path that has not shown the report.
 */
export async function approveRules(
  endpoint: string,
  refresher: TokenRefresher,
  decision: { batchId: string; ruleSha256s: string[]; expectedTestSha256: string },
): Promise<Approval> {
  const { batchId, ruleSha256s, expectedTestSha256 } = decision;
  if (
    !isHash(batchId) ||
    !isHash(expectedTestSha256) ||
    ruleSha256s.length === 0 ||
    !ruleSha256s.every(isHash) ||
    new Set(ruleSha256s).size !== ruleSha256s.length
  )
    throw new EvolveCallError(
      'invalid',
      'Choose at least one distinct rule from the reviewed test.',
    );
  return parseApproval(
    await callEvolve(
      endpoint,
      refresher,
      'evolve_rules_approve',
      {
        batch_id: batchId,
        rule_sha256s: ruleSha256s,
        expected_test_sha256: expectedTestSha256,
        confirm: true,
      },
      safe,
    ),
  );
}

/**
 * Start a NEW replenishment experiment that carries an approved rule set. Evolve resolves the hash
 * with the caller known and freezes the rules into the request; a running experiment is never
 * changed. Not retried blindly: a lost response is reported, and the same request can be sent again
 * (Evolve answers an identical start with the same receipt).
 */
export async function startWithApprovedRules(
  endpoint: string,
  refresher: TokenRefresher,
  spec: Record<string, unknown>,
  ruleSetSha256: string,
): Promise<{ experiment: string; state: string }> {
  if (!isHash(ruleSetSha256))
    throw new EvolveCallError('invalid', 'An approved rule set is a 64-character hash.');
  if (spec.pilot !== 'p1_decision')
    throw new EvolveCallError(
      'invalid',
      'Approved rule sets apply to p1_decision (replenishment) experiments only.',
    );
  if ('rule_set' in spec || 'rule_set_p_max' in spec)
    throw new EvolveCallError('invalid', 'The rules are set by the server, not the request.');
  if ('approved_rule_set_sha256' in spec)
    throw new EvolveCallError(
      'invalid',
      'The start file already names a rule set. Remove it; the approved one is attached for you.',
    );
  const raw = (await callEvolve(
    endpoint,
    refresher,
    'evolve_start',
    { ...spec, approved_rule_set_sha256: ruleSetSha256 },
    { retry: false, timeoutMs: 120_000 },
  )) as Record<string, unknown>;
  return {
    experiment: cleanText(
      typeof raw.experiment === 'string' ? raw.experiment : String(spec.experiment ?? ''),
      128,
    ),
    state: cleanText(typeof raw.state === 'string' ? raw.state : 'accepted', 40),
  };
}
