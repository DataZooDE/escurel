import { EscurelError } from './client';

/** Shown to the user for any failed gateway call: the kind first, then the message. */
export function describeError(e: unknown): string {
  if (e instanceof EscurelError) {
    switch (e.kind) {
      case 'unauthorized':
        return 'not signed in, or the token expired — run "Escurel: Sign In"';
      case 'transport':
        return `cannot reach the gateway (${e.message})`;
      case 'session_cap_reached':
        return 'the gateway has no free session slot right now; try again in a moment';
      default:
        return `${e.kind}: ${e.message}`;
    }
  }
  return e instanceof Error ? e.message : String(e);
}
