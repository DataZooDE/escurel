import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  EVOLVE_CONTROL_TARGETS,
  EVOLVE_REVIEW_CONTROLS,
  evolveTargetSkill,
  isEvolveControl,
  isEvolveReviewControl,
} from '../../src/shared/evolveControls';

describe('evolve control labels: one table', () => {
  it('keeps the review controls exactly as before the table existed', () => {
    expect([...EVOLVE_REVIEW_CONTROLS]).toEqual([
      'evolve_validate',
      'evolve_publish_candidate',
      'evolve_compare',
    ]);
    for (const label of EVOLVE_REVIEW_CONTROLS) expect(isEvolveReviewControl(label)).toBe(true);
    expect(isEvolveReviewControl('evolve_run')).toBe(false);
    expect(isEvolveReviewControl('evolve_preflight')).toBe(false);
  });

  it('names a target skill for every control and the run label', () => {
    expect(evolveTargetSkill('evolve_compare')).toBe('evolve_comparison');
    expect(evolveTargetSkill('evolve_run')).toBe('evolve_problem');
    expect(evolveTargetSkill('triage')).toBeUndefined();
    expect(isEvolveControl('evolve_prepare_source')).toBe(true);
    expect(isEvolveControl('evolve_run')).toBe(false);
  });

  it('matches the gateway table in escurel-types, label for label', () => {
    // The Rust table is the other half of the single source; they must not drift.
    const rust = readFileSync(
      join(__dirname, '../../../../crates/escurel-types/src/evolve_labels.rs'),
      'utf8',
    );
    const block = rust.slice(rust.indexOf('CONTROL_TARGETS'), rust.indexOf('];'));
    const pairs = [...block.matchAll(/\("(evolve_[a-z_]+)",\s*"(evolve_[a-z_]+)"\)/g)].map(
      (m) => [m[1], m[2]] as const,
    );
    expect(EVOLVE_CONTROL_TARGETS.map((c) => [c.label, c.targetSkill])).toEqual(pairs);
  });
});
