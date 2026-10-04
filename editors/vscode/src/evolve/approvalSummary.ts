import { createHash } from 'node:crypto';

function field(value: unknown): string {
  return value === undefined || value === null ? 'missing' : String(value);
}

function sqlDigest(value: unknown): string {
  if (typeof value !== 'string' || !value.trim()) return 'missing';
  return createHash('sha256').update(value).digest('hex');
}

/** The owner reviews this from the same exact page revision used for approval. */
export function evolveApprovalSummary(
  pageSha256: string,
  searchRequest: unknown,
  plan?: { harness?: string; steps?: ReadonlyArray<{ step: string }> },
): string {
  const spec = searchRequest && typeof searchRequest === 'object' && !Array.isArray(searchRequest)
    ? searchRequest as Record<string, unknown> : {};
  const budget = spec.budget && typeof spec.budget === 'object' && !Array.isArray(spec.budget)
    ? spec.budget as Record<string, unknown> : {};
  const service = spec.service_targets && typeof spec.service_targets === 'object'
    ? JSON.stringify(spec.service_targets) : 'missing';
  const usd = budget.max_usd === undefined ? 'NO USD CAP'
    : `${Number(budget.max_usd).toFixed(2)} USD max`;
  const steps = plan?.steps?.map((item, i) => `${i + 1}. ${item.step.trim()}`) ?? [];
  return [
    'Approve this Evolve search against the frozen problem revision?',
    ...(plan ? [`Plan harness: ${field(plan.harness)}`,
      ...(steps.length ? ['Plan steps:', ...steps] : ['Plan steps: missing'])] : []),
    `Page SHA-256: ${pageSha256}`,
    `Pilot: ${field(spec.pilot)}; holdout ID: ${field(spec.holdout_id)}`,
    `Limits: ${field(spec.max_generations)} generations; ${field(budget.max_evaluated)} evaluations; ${usd}`,
    `Service targets: ${service}`,
    `Training source SHA-256: ${field(spec.source_sha256)}`,
    `Seed SQL SHA-256: ${sqlDigest(spec.seed_sql)}`,
    `Baseline SQL SHA-256: ${sqlDigest(spec.baseline_sql)}`,
    'The plan and the private preflight are not validation of policy quality.',
    ...(plan?.harness === 'echo' ? ['Echo plans are workflow smoke tests; review the steps before authorizing a search.'] : []),
  ].join('\n');
}
