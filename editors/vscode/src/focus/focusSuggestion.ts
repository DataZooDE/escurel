/** Pure decision and asynchronous orchestration, independent of the VS Code host. */
export interface FocusSuggestionState {
  connected: boolean;
  focusActive: boolean;
  alreadySuggested: boolean;
  silentBootstrap: boolean;
}

export const SUGGESTED_KEY = 'escurel.focus.suggested';

export function shouldSuggestFocus(state: FocusSuggestionState): boolean {
  return state.connected && !state.focusActive && !state.alreadySuggested && !state.silentBootstrap;
}

export interface FocusSuggestionHost {
  state(): FocusSuggestionState;
  remember(): PromiseLike<void>;
  show(message: string, ...buttons: string[]): PromiseLike<string | undefined>;
  enter(): PromiseLike<unknown>;
}

export class FocusSuggestion {
  private claimed = false;
  constructor(private readonly host: FocusSuggestionHost) {}

  async check(): Promise<void> {
    if (this.claimed || !shouldSuggestFocus(this.host.state())) return;
    // Claim synchronously: several connection callbacks may arrive before storage finishes.
    this.claimed = true;
    await this.host.remember();
    const state = this.host.state();
    if (!state.connected || state.focusActive || state.silentBootstrap) return;
    const choice = await this.host.show(
      'Switch to the calm focus view? You can switch back at any time.',
      'Switch to focus view', 'Not now', "Don't ask again",
    );
    if (choice === 'Switch to focus view') await this.host.enter();
  }
}
