/**
 * The Evolve event labels, in ONE place (the other half is `crates/escurel-types/src/evolve_labels.rs`;
 * a unit test compares the two tables label for label).
 *
 * A control label is an owner-gated click on an Evolve page. `review` marks the ones whose click
 * must carry the exact page revision the user reviewed (`provenance.manual.expected_page_sha256`):
 * pages offer them only in the right state, the inspector never offers them (it has no verified
 * page snapshot), and the start flow re-reads the page and binds the click before capturing it. The
 * gateway re-checks all of it; these lists only keep the UI from offering a click the gateway would
 * refuse.
 */
export interface EvolveControl {
  readonly label: string;
  /** The skill the click's target page must be an instance of. */
  readonly targetSkill: string;
  readonly review: boolean;
}

/** Same rows, same order as `CONTROL_TARGETS` in escurel-types. */
export const EVOLVE_CONTROL_TARGETS: readonly EvolveControl[] = [
  { label: 'evolve_preflight', targetSkill: 'evolve_problem', review: false },
  { label: 'evolve_prepare_source', targetSkill: 'evolve_training_source', review: false },
  { label: 'evolve_validate', targetSkill: 'evolve_experiment', review: true },
  { label: 'evolve_publish_candidate', targetSkill: 'evolve_validation_report', review: true },
  { label: 'evolve_compare', targetSkill: 'evolve_comparison', review: true },
];

const RUN_LABEL = 'evolve_run';
const RUN_TARGET = 'evolve_problem';

export const EVOLVE_REVIEW_CONTROLS: readonly string[] = EVOLVE_CONTROL_TARGETS.filter(
  (c) => c.review,
).map((c) => c.label);

export function isEvolveControl(label: string): boolean {
  return EVOLVE_CONTROL_TARGETS.some((c) => c.label === label);
}

export function isEvolveReviewControl(label: string): boolean {
  return EVOLVE_REVIEW_CONTROLS.includes(label);
}

export function evolveTargetSkill(label: string): string | undefined {
  if (label === RUN_LABEL) return RUN_TARGET;
  return EVOLVE_CONTROL_TARGETS.find((c) => c.label === label)?.targetSkill;
}
