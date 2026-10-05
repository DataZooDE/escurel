import { createHash } from 'node:crypto';
import type { TokenRefresher } from '../auth/refresher';

export interface PreparedSourceBinding {
  sourceId: string;
  digest: string;
  trainingStart?: string;
  trainingEnd?: string;
}

export interface RegisteredHoldout {
  holdoutId: string;
  holdoutSha256: string;
  trainingSourceId: string;
  trainingSourceSha256: string;
  owner: string;
  endpoint: string;
}

export class EvolveRegistrationError extends Error {
  constructor(
    message: string,
    readonly conclusiveNoStore: boolean,
  ) {
    super(message);
  }
}

export function requireReviewedFileBytes(saved: Uint8Array, visible: string, dirty: boolean): void {
  if (dirty || new TextDecoder().decode(saved) !== visible)
    throw new Error(
      'The active private holdout has unsaved changes. Save the JSON file and review it before sealing.',
    );
}

export function evolveOrigin(configured: string): string {
  if (!configured)
    throw new Error(
      'Set the application setting escurel.evolveEndpoint to the Anofox Evolve origin.',
    );
  const url = new URL(configured);
  const loopback = ['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname);
  if (
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    (url.pathname !== '/' && url.pathname !== '')
  ) {
    throw new Error(
      'Evolve endpoint must be an HTTPS origin, or loopback HTTP for local development.',
    );
  }
  return url.origin;
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Private holdout must be one JSON object.');
  return value as Record<string, unknown>;
}

/** Keep outcomes in this local object; only the summary crosses into a dialog. */
export function prepareHoldout(
  value: unknown,
  source: PreparedSourceBinding,
): {
  payload: Record<string, unknown>;
  summary: string;
} {
  const payload = { ...object(value) };
  if (
    !/^[A-Za-z0-9_-]{1,128}$/.test(source.sourceId) ||
    source.sourceId.startsWith('REPLACE_') ||
    !/^[a-f0-9]{64}$/.test(source.digest)
  )
    throw new Error('Use the registered training-source ID and normalized SHA-256 from Evolve.');
  if (
    typeof payload.training_source_id === 'string' &&
    !payload.training_source_id.startsWith('REPLACE_') &&
    payload.training_source_id !== source.sourceId
  )
    throw new Error('The holdout names a different training source. Review the selected file.');
  if (
    typeof payload.training_source_sha256 === 'string' &&
    !payload.training_source_sha256.startsWith('REPLACE_') &&
    payload.training_source_sha256 !== source.digest
  )
    throw new Error('The holdout training digest differs from the prepared source.');
  payload.training_source_id = source.sourceId;
  payload.training_source_sha256 = source.digest;
  if (
    (source.trainingStart && payload.training_start !== source.trainingStart) ||
    (source.trainingEnd && payload.training_end !== source.trainingEnd)
  )
    throw new Error('Holdout training dates differ from the prepared source.');
  const id = payload.holdout_id;
  if (typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(id) || id.startsWith('REPLACE_'))
    throw new Error('Give the holdout a unique ID before registering it.');
  if (payload.evaluator_version !== 'replenishment_decision_v2')
    throw new Error('Workbench registration currently supports only replenishment_decision_v2.');
  if (
    payload.outcomes_sealed_before_search !== true ||
    typeof payload.outcomes_publicly_disclosed !== 'boolean'
  )
    throw new Error(
      'Explicitly set outcomes_sealed_before_search and outcomes_publicly_disclosed.',
    );
  if (
    payload.demand_observation !== 'true_demand' ||
    payload.inventory_as_of !== payload.holdout_start
  )
    throw new Error('Attest true demand and opening inventory as of holdout_start.');
  const problem = object(payload.problem);
  const skus = problem.skus;
  if (!Array.isArray(skus) || !skus.length || skus.length > 16)
    throw new Error('The private replay needs 1–16 SKUs.');
  const ids = skus.map((sku) => object(sku).sku_id);
  if (
    ids.some((skuId) => !Number.isSafeInteger(skuId) || (skuId as number) <= 0) ||
    new Set(ids).size !== ids.length
  )
    throw new Error('The private replay needs unique positive SKU IDs.');
  const sql = payload.baseline_sql;
  if (typeof sql !== 'string' || !sql.trim()) throw new Error('A baseline SQL policy is required.');
  const ratio = payload.max_cost_ratio;
  if (typeof ratio !== 'number' || !(ratio >= 0 && ratio < 1))
    throw new Error('V2 max_cost_ratio must be below 1.');
  const targets = object(payload.service_targets);
  const skuState = skus.map((sku) => {
    const item = object(sku);
    const pipeline = Array.isArray(item.initial_pipeline) ? item.initial_pipeline : [];
    return {
      sku_id: item.sku_id,
      opening_stock: item.initial_stock,
      pipeline_days: pipeline.length,
      pipeline_units: pipeline.reduce<number>(
        (sum, qty) => sum + (typeof qty === 'number' ? qty : 0),
        0,
      ),
      demand_days: Array.isArray(item.demand) ? item.demand.length : 'missing',
      lead_time: item.lead_time,
      case_pack: item.case_pack,
      min_order: item.min_order,
      holding_cost: item.holding_cost,
      fixed_order_cost: item.fixed_order_cost,
      shortage_cost: item.shortage_cost,
    };
  });
  const summary = [
    `Holdout: ${id}`,
    `Training source: ${source.sourceId}; normalized SHA-256: ${source.digest}`,
    `Operator-declared holdout source: ${String(payload.source_ref)}; SHA-256: ${String(payload.source_sha256)}`,
    `Training: ${String(payload.training_start)} to ${String(payload.training_end)}`,
    `History: ${String(payload.history_start)} to ${String(payload.history_end)}`,
    `Holdout: ${String(payload.holdout_start)} to ${String(payload.holdout_end)}; inventory as of ${String(payload.inventory_as_of)}`,
    `SKUs: ${ids.join(', ')}; shared capacity: ${String(problem.capacity)}`,
    `Per-SKU opening state, constraints, outcome coverage, and economics: ${JSON.stringify(skuState)}`,
    `Baseline SQL SHA-256: ${createHash('sha256').update(sql).digest('hex')}`,
    `Baseline rule preview: ${sql.replace(/\s+/g, ' ').slice(0, 240)}${sql.length > 240 ? '…' : ''}`,
    `Aggregate service target: ${String(targets.aggregate_min_fill_rate)}; per-SKU targets: ${JSON.stringify(targets.per_sku_min_fill_rate)}`,
    `Maximum cost ratio: ${ratio}; planning/scored days: ${String(payload.planning_window_days)}/${String(payload.scored_window_days)}; tails: ${JSON.stringify(payload.sensitivity_tail_days)}`,
    `Unit costs: ${JSON.stringify(payload.unit_order_costs)}; terminal tolerances: ${JSON.stringify(payload.terminal_stock_tolerance)}`,
    `Publicly disclosed outcomes: ${String(payload.outcomes_publicly_disclosed)}`,
    'The registered declaration becomes immutable under this holdout ID. Inspect the local file for full pipeline and policy details before sealing.',
    'This is one operator-attested episode. Source lineage, true demand, opening inventory, and whether outcomes were unseen are not independently verified.',
    ...(payload.outcomes_publicly_disclosed
      ? ['Publicly disclosed synthetic fixture: sandbox evidence only.']
      : []),
  ].join('\n');
  return { payload, summary };
}

/** Validate the metadata-only template; dated demand rows stay in the CSV. */
export function prepareHoldoutCsv(
  value: unknown,
  source: PreparedSourceBinding,
): {
  template: Record<string, unknown>;
  templateJson: string;
  summary: string;
} {
  const template = { ...object(value) };
  if (
    !/^[A-Za-z0-9_-]{1,128}$/.test(source.sourceId) ||
    source.sourceId.startsWith('REPLACE_') ||
    !/^[a-f0-9]{64}$/.test(source.digest)
  )
    throw new Error('Use the registered training-source ID and normalized SHA-256 from Evolve.');
  if (
    typeof template.training_source_id === 'string' &&
    !template.training_source_id.startsWith('REPLACE_') &&
    template.training_source_id !== source.sourceId
  )
    throw new Error('The holdout names a different training source. Review the selected template.');
  if (
    typeof template.training_source_sha256 === 'string' &&
    !template.training_source_sha256.startsWith('REPLACE_') &&
    template.training_source_sha256 !== source.digest
  )
    throw new Error('The holdout training digest differs from the prepared source.');
  template.training_source_id = source.sourceId;
  template.training_source_sha256 = source.digest;
  if (
    (source.trainingStart && template.training_start !== source.trainingStart) ||
    (source.trainingEnd && template.training_end !== source.trainingEnd)
  )
    throw new Error('Holdout training dates differ from the prepared source.');
  if ('source_sha256' in template)
    throw new Error(
      'Omit source_sha256 from a CSV template; Evolve derives it from the exact CSV bytes.',
    );
  const id = template.holdout_id;
  if (typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(id) || id.startsWith('REPLACE_'))
    throw new Error('Give the holdout a unique ID before registering it.');
  if (template.evaluator_version !== 'replenishment_decision_v2')
    throw new Error('Workbench registration currently supports only replenishment_decision_v2.');
  if (
    template.outcomes_sealed_before_search !== true ||
    typeof template.outcomes_publicly_disclosed !== 'boolean'
  )
    throw new Error(
      'Explicitly set outcomes_sealed_before_search and outcomes_publicly_disclosed.',
    );
  if (
    template.demand_observation !== 'true_demand' ||
    template.inventory_as_of !== template.holdout_start
  )
    throw new Error('Attest true demand and opening inventory as of holdout_start.');
  const problem = object(template.problem);
  const skus = problem.skus;
  if (!Array.isArray(skus) || !skus.length || skus.length > 16)
    throw new Error('The private replay needs 1–16 SKUs.');
  const ids = skus.map((sku) => object(sku).sku_id);
  if (
    ids.some((skuId) => !Number.isSafeInteger(skuId) || (skuId as number) <= 0) ||
    new Set(ids).size !== ids.length
  )
    throw new Error('The private replay needs unique positive SKU IDs.');
  if (skus.some((sku) => 'demand' in object(sku) || 'history' in object(sku)))
    throw new Error(
      'Keep demand and history out of the template; Evolve derives both from the dated CSV.',
    );
  const skuState = skus.map((sku) => {
    const item = object(sku);
    const pipeline = Array.isArray(item.initial_pipeline) ? item.initial_pipeline : [];
    return {
      sku_id: item.sku_id,
      opening_stock: item.initial_stock,
      pipeline_days: pipeline.length,
      pipeline_units: pipeline.reduce<number>(
        (sum, qty) => sum + (typeof qty === 'number' ? qty : 0),
        0,
      ),
      lead_time: item.lead_time,
      case_pack: item.case_pack,
      min_order: item.min_order,
      holding_cost: item.holding_cost,
      fixed_order_cost: item.fixed_order_cost,
      shortage_cost: item.shortage_cost,
    };
  });
  const baseline = typeof template.baseline_sql === 'string' ? template.baseline_sql : '';
  const summary = [
    `Holdout: ${id}; training source: ${source.sourceId}; normalized SHA-256: ${source.digest}`,
    `Training: ${String(template.training_start)} to ${String(template.training_end)}; history: ${String(template.history_start)} to ${String(template.history_end)}`,
    `Holdout: ${String(template.holdout_start)} to ${String(template.holdout_end)}; inventory as of ${String(template.inventory_as_of)}`,
    `SKUs: ${ids.join(', ')}; capacity: ${String(problem.capacity)}; opening stock, pipeline, constraints and costs: ${JSON.stringify(skuState)}`,
    `Baseline SQL SHA-256: ${createHash('sha256').update(baseline).digest('hex')}; preview: ${baseline.replace(/\s+/g, ' ').slice(0, 240)}${baseline.length > 240 ? '…' : ''}`,
    `Service targets: ${JSON.stringify(template.service_targets)}; max cost ratio: ${String(template.max_cost_ratio)}; planning/scored days: ${String(template.planning_window_days)}/${String(template.scored_window_days)}; tails: ${JSON.stringify(template.sensitivity_tail_days)}`,
    `Publicly disclosed outcomes: ${String(template.outcomes_publicly_disclosed)}; demand observation: ${String(template.demand_observation)}`,
    'Holdout outcomes come from the selected CSV and are never shown in this review dialog.',
  ].join('\n');
  const templateJson = JSON.stringify(template, null, 2) + '\n';
  return { template, templateJson, summary };
}

export async function registerHoldoutAtEvolve(
  endpoint: string,
  refresher: TokenRefresher,
  payload: Record<string, unknown>,
): Promise<{ holdoutId: string; holdoutSha256: string }> {
  const origin = evolveOrigin(endpoint);
  const body = JSON.stringify(payload);
  let lastStatus = 0;
  for (let attempt = 0; attempt < 3; attempt++) {
    const token =
      attempt === 1 && lastStatus === 401 ? await refresher.invalidate() : await refresher.get();
    if (!token)
      throw new Error(
        'Sign in with an OIDC token accepted by Evolve before registering private outcomes.',
      );
    let response: Response;
    try {
      response = await fetch(origin + '/', {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.timeout(20_000),
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          'X-Triton-Tool': 'evolve_register_holdout',
        },
        body,
      });
    } catch {
      if (attempt < 2) continue;
      throw new Error(
        'Evolve did not confirm registration. Retry the same reviewed declaration and holdout ID; Evolve will return its sealed digest if it already stored this declaration.',
      );
    }
    lastStatus = response.status;
    if (response.status === 401 && attempt === 0) continue;
    if (response.status === 503 && attempt < 2) continue;
    if (response.status === 409)
      throw new Error(
        'This holdout ID is already sealed with a different declaration. Review the existing registration or choose a new ID.',
      );
    if (!response.ok) {
      if (response.status === 401)
        throw new EvolveRegistrationError(
          'Evolve rejected the signed-in token. Check its OIDC audience and sign in again.',
          true,
        );
      if (response.status === 422) {
        let reason: unknown;
        try {
          reason = object(await response.json()).reason;
        } catch {
          /* No trusted detail. */
        }
        if (reason === 'invalid_training_source_binding')
          throw new EvolveRegistrationError(
            'Evolve rejected the training-source binding. Check source ID, normalized digest, training dates, and signed-in owner.',
            true,
          );
        throw new EvolveRegistrationError(
          'Evolve rejected the V2 declaration. Check dates, true-demand attestation, SKU economics, service targets, baseline SQL, and both continuation tails.',
          true,
        );
      }
      if (response.status === 403)
        throw new EvolveRegistrationError(
          'Evolve denied this owner. Sign in with the same tenant and subject used to prepare the training source.',
          true,
        );
      throw new Error(
        `Evolve rejected registration (HTTP ${response.status}). Check the private declaration and owner/source binding.`,
      );
    }
    const result = object(await response.json());
    if (
      result.holdout_id !== payload.holdout_id ||
      result.state !== 'sealed' ||
      typeof result.holdout_sha256 !== 'string' ||
      !/^[a-f0-9]{64}$/.test(result.holdout_sha256)
    )
      throw new Error('Evolve returned an invalid holdout registration receipt.');
    return { holdoutId: result.holdout_id as string, holdoutSha256: result.holdout_sha256 };
  }
  throw new Error(
    'Evolve registration did not complete. Retry the same local file and holdout ID.',
  );
}

export async function registerHoldoutCsvAtEvolve(
  endpoint: string,
  refresher: TokenRefresher,
  payload: { manifest_json: string; template_json: string; daily_demand_csv: string },
): Promise<{ holdoutId: string; holdoutSha256: string }> {
  const origin = evolveOrigin(endpoint);
  const body = JSON.stringify(payload);
  let lastStatus = 0;
  for (let attempt = 0; attempt < 3; attempt++) {
    const token =
      attempt === 1 && lastStatus === 401 ? await refresher.invalidate() : await refresher.get();
    if (!token)
      throw new Error(
        'Sign in with an OIDC token accepted by Evolve before registering private outcomes.',
      );
    let response: Response;
    try {
      response = await fetch(origin + '/', {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.timeout(60_000),
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          'X-Triton-Tool': 'evolve_register_holdout_csv',
        },
        body,
      });
    } catch {
      if (attempt < 2) continue;
      throw new Error(
        'Evolve did not confirm registration. Retry the same three reviewed files and holdout ID; Evolve returns its sealed receipt if it already stored them.',
      );
    }
    lastStatus = response.status;
    if (response.status === 401 && attempt === 0) continue;
    if (response.status === 503 && attempt < 2) continue;
    if (response.status === 409)
      throw new Error(
        'This holdout ID is already sealed with different CSV inputs. Restore the reviewed files or choose a new ID.',
      );
    if (!response.ok) {
      if (response.status === 401)
        throw new EvolveRegistrationError(
          'Evolve rejected the signed-in token. Check its OIDC audience and sign in again.',
          true,
        );
      if (response.status === 403)
        throw new EvolveRegistrationError(
          'Evolve denied this owner. Sign in with the same tenant and subject used to prepare the training source.',
          true,
        );
      if (response.status === 422) {
        let reason = '';
        try {
          reason = String(object(await response.json()).error ?? '').toLowerCase();
        } catch {
          /* Untrusted detail is not shown. */
        }
        const guidance = reason.includes('daily_demand_sha256')
          ? 'The manifest digest does not match the selected CSV. Select the matching manifest or export again.'
          : reason.includes('training source') || reason.includes('source binding')
            ? 'The registered training source or owner does not match this template. Reprepare the training source and update its ID and digest.'
            : reason.includes('date') ||
                reason.includes('history') ||
                reason.includes('holdout window')
              ? 'Check that history and holdout dates are contiguous, after training, fully covered by the CSV, and before extracted_at.'
              : reason.includes('sku') || reason.includes('coverage') || reason.includes('row')
                ? 'Check the exact CSV header, unique SKU/day rows, full date coverage for every SKU, units, and true-demand/fulfilled rules.'
                : 'Check the manifest, metadata template, exact CSV header, full date coverage, units, and true-demand fields.';
        throw new EvolveRegistrationError(
          `Evolve rejected the dated CSV bundle. ${guidance}`,
          true,
        );
      }
      throw new Error(
        `Evolve rejected registration (HTTP ${response.status}). Retry only with the same reviewed files and holdout ID.`,
      );
    }
    const result = object(await response.json());
    if (
      result.holdout_id !== object(JSON.parse(payload.template_json)).holdout_id ||
      result.state !== 'sealed' ||
      typeof result.holdout_sha256 !== 'string' ||
      !/^[a-f0-9]{64}$/.test(result.holdout_sha256)
    )
      throw new Error('Evolve returned an invalid holdout CSV registration receipt.');
    return { holdoutId: result.holdout_id as string, holdoutSha256: result.holdout_sha256 };
  }
  throw new Error(
    'Evolve registration did not complete. Retry the same local files and holdout ID.',
  );
}
