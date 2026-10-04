// What the run controls say to the person, as pure functions (no `vscode`).
import type { ControlRequest } from './controls';

type Action = ControlRequest['action'];

/** `status`: one line in the status bar that expires. `toast`: needs the person's eyes. */
export function outcomeChannel(outcome: string): 'status' | 'toast' {
  switch (outcome) {
    case 'cancelled':
    case 'requeued':
    case 'paused':
    case 'resumed':
      return 'status';
    default:
      return 'toast';
  }
}

export interface Confirmation {
  message: string;
  detail: string;
  button: string;
}

/** The questions asked before an action that cannot be taken back. Undoable actions are not asked. */
export function confirmationFor(action: Action): Confirmation | undefined {
  switch (action) {
    case 'cancel':
      return {
        message: 'Cancel this run?',
        detail: 'Work already done is kept. The run stops and shows as cancelled.',
        button: 'Cancel run',
      };
    case 'pause':
      return {
        message: 'Pause agents for the whole tenant?',
        detail: 'New work waits until an admin resumes them. Runs in progress finish.',
        button: 'Pause agents',
      };
    default:
      return undefined;
  }
}

/** The one line shown while the runner picks the request up. */
export function progressTitle(action: Action): string {
  switch (action) {
    case 'cancel':
      return 'Cancelling…';
    case 'retry':
      return 'Retrying: starts a new run, this attempt stays in history';
    case 'requeue':
      return 'Requeueing…';
    case 'pause':
      return 'Pausing agents…';
    case 'resume':
      return 'Resuming agents…';
  }
}
