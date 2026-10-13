// Which harness a manual start asks for: pure, no `vscode` (unit-tested; the provider owns the UI).
import { allowedHarnesses, type RunnerStatusBody } from '../views/runnerModel';

export type HarnessChoice =
  /** Send this harness in `provenance.manual.harness`. */
  | { kind: 'send'; harness: string }
  /** Offer the published allow-list in a QuickPick; `defaultHarness` is preselected. */
  | { kind: 'ask'; harnesses: string[]; defaultHarness: string | undefined }
  /** The configured harness is not on the runner's published list: the caller must say so, never send it. */
  | { kind: 'refused'; configured: string; allowed: string[] }
  /** Send no harness: the runner uses its default. */
  | { kind: 'none' };

/**
 * The harness a start sends, from the `escurel.harness` setting and the runner's published
 * allow-list (BACKEND_GAPS PR-10):
 *
 * - a configured harness is sent when the list is unknown (an old runner: today's behaviour) or
 *   names it; when the published list does NOT contain it the start is `refused` (the caller tells
 *   the user and lets them choose: silently running another engine would be a surprise);
 * - a blank setting asks ONLY when the list is known and has more than one entry (nothing to
 *   choose with zero or one); the runner's default is preselected when it is on the list;
 * - anything else sends none, and the runner runs its default. Nothing is remembered.
 */
export function chooseHarness(
  settingsHarness: string | undefined,
  status: RunnerStatusBody | null | undefined,
): HarnessChoice {
  const configured = (settingsHarness ?? '').trim();
  const allow = allowedHarnesses(status);
  if (configured) {
    if (allow && !allow.includes(configured)) return { kind: 'refused', configured, allowed: allow };
    return { kind: 'send', harness: configured };
  }
  if (!allow || allow.length <= 1) return { kind: 'none' };
  const fallback = allow[0] as string;
  return {
    kind: 'ask',
    harnesses: allow,
    defaultHarness:
      status?.harness && allow.includes(status.harness) ? status.harness : fallback,
  };
}
