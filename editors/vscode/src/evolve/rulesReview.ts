import { EvolveCallError } from './rulesClient';
import {
  approvable,
  isHash,
  renderReport,
  type Approval,
  type Evidence,
  type Proposal,
  type TestReport,
} from './rulesModel';

/** What the review needs from Evolve; the command binds it to the endpoint and the signed-in token. */
export interface RulesApi {
  propose(sourceId: string): Promise<Proposal>;
  test(batchId: string): Promise<TestReport>;
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
  /** Testing a batch spends the multiple-testing correction, so it is asked first. */
  confirmTest(proposal: Proposal): Promise<boolean>;
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
  if (!(await ui.confirmTest(proposal))) return { state: 'cancelled' };

  const report = await api.test(proposal.batchId);
  const proposed = new Set(proposal.candidates.map((c) => c.sha256));
  if (
    report.batchId !== proposal.batchId ||
    report.evidence.some((e) => !proposed.has(e.ruleSha256))
  )
    throw stale('The test does not belong to the batch that was proposed. Start the review again.');

  await ui.showReport(renderReport(report));
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
    ...rules.map((e) => `- ${e.text} (${e.confirmation.rows} confirmation rows, p ${e.pValue})`),
    '',
    `Bound to test ${report.testSha256}.`,
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
