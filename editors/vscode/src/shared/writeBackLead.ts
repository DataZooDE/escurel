import type { WriteBackOutcome } from './writeBack';

/** The bold lead word of a write-back status line, so no outcome is told by colour alone. */
export function writeBackLead(outcome: WriteBackOutcome): {
  word: string;
  tone: 'ok' | 'problem' | 'pending';
} {
  switch (outcome) {
    case 'applied':
      return { word: 'Applied', tone: 'ok' };
    case 'applying':
      return { word: 'Sending', tone: 'pending' };
    case 'failed':
      return { word: 'Failed', tone: 'problem' };
    case 'rejected':
      return { word: 'Rejected', tone: 'problem' };
    case 'conflict':
      return { word: 'Conflict', tone: 'problem' };
  }
}
