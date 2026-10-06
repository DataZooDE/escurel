/**
 * Evolve review controls: actions whose click must carry the exact page revision the user
 * reviewed (`provenance.manual.expected_page_sha256`). Pages offer them only in the right
 * state, the inspector never offers them (it has no verified page snapshot), and the start
 * flow re-reads the page and binds the click before capturing it. The gateway re-checks all
 * of it; these lists only keep the UI from offering a click the gateway would refuse.
 */
export const EVOLVE_REVIEW_CONTROLS: readonly string[] = [
  'evolve_validate',
  'evolve_publish_candidate',
  'evolve_compare',
];

export function isEvolveReviewControl(label: string): boolean {
  return EVOLVE_REVIEW_CONTROLS.includes(label);
}
