import { EvolveCallError } from './rulesClient';
import { isHash, renderApproval, type Approval } from './rulesModel';

/** What the "show approved rules" flow needs from the host: ask for a hash, show a document. */
export interface ApprovedUi {
  /** A hash chosen from the ones remembered this session, or typed; undefined when cancelled. */
  pickHash(remembered: string[]): Promise<string | undefined>;
  show(text: string): Promise<void>;
}

/**
 * Read one approved rule set and show it. Needs a signed-in owner (Evolve answers only for the owner
 * who approved the set) and checks that owner did not change while the set was read. `presetHash`
 * skips the question (the "View approved rule set" action on the approval notice).
 */
export async function showApprovedRules(
  deps: {
    ui: ApprovedUi;
    get: (ruleSetSha256: string) => Promise<Approval>;
    subject: () => Promise<string | undefined>;
    remembered: (owner: string) => string[];
  },
  presetHash?: string,
): Promise<Approval | undefined> {
  const { ui, get, subject, remembered } = deps;
  const owner = await subject();
  if (!owner)
    throw new Error('Sign in before reading approved rules: Evolve binds them to your identity.');
  const asked = presetHash ?? (await ui.pickHash(remembered(owner)));
  if (asked === undefined) return undefined;
  const wanted = asked.trim().toLowerCase();
  if (!isHash(wanted))
    throw new EvolveCallError('invalid', 'An approved rule set is a 64-character hash.');
  let approval: Approval;
  try {
    approval = await get(wanted);
  } catch (error) {
    if (error instanceof EvolveCallError && error.kind === 'not_found')
      throw new EvolveCallError(
        'not_found',
        `Approved rule set ${wanted.slice(0, 12)}… was not found for the signed-in user. Check the hash; Evolve shows a set only to the owner who approved it.`,
      );
    throw error;
  }
  if ((await subject()) !== owner)
    throw new Error('The signed-in owner changed while the rules were read. Sign in again.');
  await ui.show(renderApproval(approval));
  return approval;
}
