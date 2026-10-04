import { EscurelError } from './client';
import { cleanText } from './shared/untrustedText';

/** One line of an error message: anything after the first line is a stack or detail, kept in the log. */
const oneLine = (text: string, max?: number): string => cleanText(text.split('\n')[0] ?? '', max);

/** Shown to the user for any failed gateway call: a sentence, never a raw code or unbounded text. */
export function describeError(e: unknown): string {
  if (e instanceof EscurelError) {
    switch (e.kind) {
      case 'unauthorized':
        return 'not signed in, or the token expired — run "Escurel: Sign in"';
      case 'transport':
        return `cannot reach the gateway (${oneLine(e.message, 160)})`;
      case 'session_cap_reached':
        return 'the gateway has no free session slot right now; try again in a moment';
      case 'forbidden':
        return 'you are not allowed to do that here; ask an admin if you need access';
      case 'quota_exhausted':
        return 'the gateway is limiting requests right now; try again in a moment';
      case 'tenant_suspended':
        return 'this tenant is suspended; ask an operator';
      case 'tenant_quarantined':
        return 'this tenant is waiting for a data migration (the page format changed); ask an operator to run `escurel admin migrate-kind`';
      case 'server_incompatible':
        return 'this gateway is older than this extension (it uses a data format the extension no longer reads); update the gateway or use an older extension';
      default:
        return oneLine(`${e.kind}: ${e.message}`);
    }
  }
  return oneLine(e instanceof Error ? e.message : String(e));
}

/** What a failed load says about the connection, for the views' welcome states. */
export type ConnectionState =
  'unauthorized' | 'quarantined' | 'incompatible' | 'unreachable' | 'error';

export function connectionStateOf(e: unknown): ConnectionState {
  if (!(e instanceof EscurelError)) return 'error';
  switch (e.kind) {
    case 'unauthorized':
      return 'unauthorized';
    case 'tenant_quarantined':
      return 'quarantined';
    case 'server_incompatible':
      return 'incompatible';
    case 'transport':
      return 'unreachable';
    default:
      return 'error';
  }
}
