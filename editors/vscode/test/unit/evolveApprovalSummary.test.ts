import { describe, expect, it } from 'vitest';
import { createHash } from 'node:crypto';
import { evolveApprovalSummary } from '../../src/evolve/approvalSummary';

describe('Evolve approval summary', () => {
  it('shows the frozen limits, targets, holdout and exact SQL identities before approval', () => {
    const seed = 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation';
    const baseline = 'SELECT sku_id, 0::BIGINT AS order_qty FROM p1_observation';
    const summary = evolveApprovalSummary('a'.repeat(64), {
      pilot: 'p1_decision', holdout_id: 'private-1', source_sha256: 'b'.repeat(64),
      max_generations: 3, budget: { max_evaluated: 8, max_usd: 2.5 },
      service_targets: { aggregate_min_fill_rate: 0.95 }, seed_sql: seed, baseline_sql: baseline,
    });
    expect(summary).toContain('a'.repeat(64));
    expect(summary).toContain('private-1');
    expect(summary).toContain('3 generations; 8 evaluations; 2.50 USD max');
    expect(summary).toContain('aggregate_min_fill_rate');
    expect(summary).toContain(createHash('sha256').update(seed).digest('hex'));
    expect(summary).toContain(createHash('sha256').update(baseline).digest('hex'));
    expect(summary).not.toContain(seed);
  });

  it('states when the search has no USD cap', () => {
    expect(evolveApprovalSummary('a'.repeat(64), { budget: { max_evaluated: 1 } }))
      .toContain('NO USD CAP');
  });

  it('shows the actual plan steps and marks an echo plan as a workflow smoke test', () => {
    const summary = evolveApprovalSummary('a'.repeat(64), { pilot: 'p1_decision' }, {
      harness: 'echo', steps: [
        { step: 'Fold the event; no policy reasoning.' },
        { step: 'Second step' }, { step: 'Third step' }, { step: 'Fourth step' },
      ],
    });
    expect(summary).toContain('Plan harness: echo');
    expect(summary).toContain('1. Fold the event; no policy reasoning.');
    expect(summary).toContain('4. Fourth step');
    expect(summary).toContain('Echo plans are workflow smoke tests');
  });
});
