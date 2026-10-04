// The state chip of a thread card: a short word and the shape of an icon, so a state never rests on
// colour alone and never shows the wire word ('processed', 'inbox').

export type ChipIcon = 'check' | 'cross' | 'sync' | 'clock' | 'none';

export interface ChipWords {
  text: string;
  icon: ChipIcon;
}

export function chipWords(state: string): ChipWords {
  switch (state) {
    case 'processed':
      return { text: 'done', icon: 'check' };
    case 'promoted':
      return { text: 'applied', icon: 'check' };
    case 'inbox':
      return { text: 'waiting', icon: 'clock' };
    case 'open':
      return { text: 'open', icon: 'clock' };
    case 'planned':
      return { text: 'plan ready', icon: 'clock' };
    case 'running':
      return { text: 'running', icon: 'sync' };
    case 'failed':
      return { text: 'failed', icon: 'cross' };
    case 'dead_letter':
      return { text: 'gave up', icon: 'cross' };
    case 'cancelled':
      return { text: 'cancelled', icon: 'cross' };
    case 'discarded':
      return { text: 'discarded', icon: 'cross' };
    default:
      return { text: state.replaceAll('_', ' '), icon: 'none' };
  }
}
