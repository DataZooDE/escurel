import { EvolveCallError } from './rulesClient';
import {
  approvable,
  describePolicy,
  evidenceFacts,
  ALPHA_CAP,
  P_MAX_CAP,
  isHash,
  percent,
  renderReport,
  type Approval,
  type Evidence,
  type PolicyChoice,
  type Proposal,
  type TestReport,
} from './rulesModel';

/** What the review needs from Evolve; the command binds it to the endpoint and the signed-in token. */
export interface RulesApi {
  propose(sourceId: string): Promise<Proposal>;
  test(batchId: string, choice: PolicyChoice): Promise<TestReport>;
  get(batchId: string): Promise<{ test: TestReport | undefined }>;
  approve(decision: {
    batchId: string;
    ruleSha256s: string[];
    expectedTestSha256: string;
  }): Promise<Approval>;
}

/** Every question a person is asked. Each returns what they answered; nothing is assumed. */
export interface RulesUi {
  askSourceId(): Promise<string | undefined>;
  /**
   * Optionally choose p_max and alpha for the test (within the service's caps). An empty choice means
   * the service default; undefined means the person cancelled. Not asked for state batches.
   */
  askPolicy(proposal: Proposal): Promise<PolicyChoice | undefined>;
  /** Testing a batch spends the multiple-testing correction, so it is asked first. */
  confirmTest(proposal: Proposal, choice: PolicyChoice): Promise<boolean>;
  showReport(markdown: string): Promise<void>;
  /** The supported rules to choose from; resolves the hashes chosen, or undefined on cancel. */
  pickRules(supported: Evidence[]): Promise<string[] | undefined>;
  confirmApproval(summary: string): Promise<boolean>;
  notify(message: string): void;
}

export type ReviewOutcome =
  | { state: 'approved'; approval: Approval; sourceId: string; batchId: string; testSha256: string }
  | { state: 'nothing_to_approve'; report: TestReport }
  | { state: 'cancelled' };

const stale = (what: string) => new EvolveCallError('stale', what);

/**
 * propose, test, review, choose, confirm, approve. Approval is never automatic: it is reached only
 * through two explicit answers (the rules chosen, then the confirmation of exactly those rules for
 * exactly that test), and it is checked against the stored test and the signed-in owner at the
 * moment of approval, so a changed batch, a changed owner or a re-run test stops it.
 */
export async function reviewRules(deps: {
  ui: RulesUi;
  api: RulesApi;
  subject: () => Promise<string | undefined>;
}): Promise<ReviewOutcome> {
  const { ui, api, subject } = deps;
  const owner = await subject();
  if (!owner)
    throw new Error('Sign in before reviewing rules: Evolve binds them to your identity.');
  const sourceId = (await ui.askSourceId())?.trim();
  if (!sourceId) return { state: 'cancelled' };

  const proposal = await api.propose(sourceId);
  if (proposal.candidates.length === 0) {
    ui.notify('Evolve found no candidate rules in this training source.');
    return { state: 'cancelled' };
  }
  const choice = proposal.kind === 'state' ? {} : await ui.askPolicy(proposal);
  if (!choice) return { state: 'cancelled' };
  if (!(await ui.confirmTest(proposal, choice))) return { state: 'cancelled' };

  const report = await api.test(proposal.batchId, choice);
  const proposed = new Set(proposal.candidates.map((c) => c.sha256));
  if (
    report.batchId !== proposal.batchId ||
    report.evidence.some((e) => !proposed.has(e.ruleSha256))
  )
    throw stale('The test does not belong to the batch that was proposed. Start the review again.');

  // A choice that did not take effect is either a batch tested before (the stored test stands and the
  // document says so) or a different policy than asked for (a fault: nothing is shown or approved).
  if (
    !report.alreadyTested &&
    ((choice.pMax !== undefined && report.policy.pMax !== choice.pMax) ||
      (choice.alpha !== undefined && report.policy.alpha !== choice.alpha))
  )
    throw stale(
      'The test was made under a different policy than the one you chose. Nothing was approved.',
    );
  if (report.alreadyTested && (choice.pMax !== undefined || choice.alpha !== undefined)) {
    const same =
      (choice.pMax === undefined || choice.pMax === report.policy.pMax) &&
      (choice.alpha === undefined || choice.alpha === report.policy.alpha);
    if (!same)
      ui.notify(
        `This batch was already tested under ${describePolicy(report.policy)}; your choice (${[
          choice.pMax !== undefined ? `p_max ${percent(choice.pMax)}` : '',
          choice.alpha !== undefined ? `alpha ${percent(choice.alpha)}` : '',
        ]
          .filter(Boolean)
          .join(', ')}) was not applied.`,
      );
  }
  await ui.showReport(renderReport(report, { requested: choice }));
  const offered = approvable(report);
  if (offered.length === 0) return { state: 'nothing_to_approve', report };

  const chosen = await ui.pickRules(offered);
  if (!chosen || chosen.length === 0) return { state: 'cancelled' };
  const supported = new Set(offered.map((e) => e.ruleSha256));
  if (!chosen.every((h) => isHash(h) && supported.has(h)))
    throw new Error('A chosen rule is not one of the supported rules of this test.');
  const rules = offered.filter((e) => chosen.includes(e.ruleSha256));

  const summary = [
    `Approve ${rules.length} rule${rules.length === 1 ? '' : 's'} for the training source ${sourceId}?`,
    '',
    ...rules.map((e) => `- ${e.text} (${evidenceFacts(e)})`),
    '',
    `Bound to test ${report.testSha256}, under ${describePolicy(report.policy)}.`,
    'The upper bounds in the report are informational: they do not decide anything and are not an approval signal.',
    'They apply to new experiments only, started with the approved hash; a running experiment is never changed. Approved means they survived a pre-registered test on this training data, not that they are true of the world.',
    `Your approval is recorded as ${owner}.`,
  ].join('\n');
  if (!(await ui.confirmApproval(summary))) return { state: 'cancelled' };

  if ((await subject()) !== owner)
    throw new Error('The signed-in owner changed during the review. Sign in again and repeat it.');
  const current = (await api.get(proposal.batchId)).test;
  if (!current || current.testSha256 !== report.testSha256)
    throw stale('The stored test is no longer the one you reviewed. Start the review again.');

  const approval = await api.approve({
    batchId: proposal.batchId,
    ruleSha256s: rules.map((e) => e.ruleSha256),
    expectedTestSha256: report.testSha256,
  });
  return {
    state: 'approved',
    approval,
    sourceId,
    batchId: proposal.batchId,
    testSha256: report.testSha256,
  };
}

/**
 * Start a new experiment from a start file, with the approved rule set attached. The file is the
 * person's own; this adds the hash, checks the file names the same source and data the rules were
 * approved on, and asks once more before anything is spent.
 */
export async function startWithRules(deps: {
  approval: Approval;
  sourceId: string;
  readSpec: () => Promise<Record<string, unknown> | undefined>;
  confirmStart: (summary: string) => Promise<boolean>;
  start: (
    spec: Record<string, unknown>,
    ruleSetSha256: string,
  ) => Promise<{ experiment: string; state: string }>;
}): Promise<{ experiment: string; state: string } | undefined> {
  const spec = await deps.readSpec();
  if (!spec) return undefined;
  if (typeof spec.experiment !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(spec.experiment))
    throw new Error('The start file needs a new experiment id (letters, digits, - and _).');
  if (spec.training_source_id !== deps.sourceId)
    throw new Error(
      'The start file names a different training source than the rules were approved on.',
    );
  if (typeof spec.source_sha256 === 'string' && spec.source_sha256 !== deps.approval.dataSha256)
    throw new Error('The start file is for different data than the rules were approved on.');
  const summary = [
    `Start new experiment ${spec.experiment} (${String(spec.pilot)}) on ${deps.sourceId}?`,
    `It carries the approved rule set ${deps.approval.ruleSetSha256} (${deps.approval.rules.length} rule${deps.approval.rules.length === 1 ? '' : 's'}).`,
    'Evolve checks the rules against this experiment’s training data first and refuses the start if one is breached.',
    'This starts a search directly in Evolve and may spend its budget; it does not go through the Escurel plan review.',
  ].join('\n');
  if (!(await deps.confirmStart(summary))) return undefined;
  return deps.start(spec, deps.approval.ruleSetSha256);
}

/** The chosen policy in words, for the modal that precedes the one-shot test. */
export function describeChoice(choice: PolicyChoice): string {
  const part = (name: string, value: number | undefined) =>
    value === undefined ? `${name} the service default` : `${name} ${percent(value)} (${value})`;
  return `${part('p_max', choice.pMax)}, ${part('alpha', choice.alpha)}`;
}

/** What the confirmation before the one-shot test says: the policy, its caps and that it cannot be redone. */
export function confirmTestDetail(proposal: Proposal, choice: PolicyChoice): string {
  const state = proposal.kind === 'state';
  return [
    `Batch ${proposal.batchId}`,
    state
      ? 'These rules are tested by replaying known programs on this exact instance; there is no p_max or alpha.'
      : `Found in the first ${proposal.dayCut} of ${proposal.days} training days; they are tested on the rest.`,
    ...(state
      ? []
      : [
          '',
          `Test policy: ${describeChoice(choice)}.`,
          `Caps: p_max up to ${percent(P_MAX_CAP)}, alpha up to ${percent(ALPHA_CAP)}; the service enforces them too.`,
        ]),
    '',
    ...proposal.candidates.slice(0, 25).map((c) => `- ${c.text}`),
    ...(proposal.candidates.length > 25 ? [`… and ${proposal.candidates.length - 25} more`] : []),
    '',
    'This test is one-shot per batch. The result is stored and cannot be redone: asking again returns the stored report, not a new test, so a wrong p_max or alpha cannot be corrected afterwards. Every candidate also counts toward the multiple-testing correction for this data.',
  ].join('\n');
}
