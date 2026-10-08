import * as vscode from 'vscode';
import { log } from '../log';
import { FOCUS_SETTINGS, planEnter, planExit, type SavedSettings } from './focusSettings';

const SAVED_KEY = 'escurel.focus.restore';
const CONTEXT_KEY = 'escurel.focusMode';

export interface EnterOptions {
  /** Skip the question (the demo, tests, an explicit "switch" button). */
  silent?: boolean;
  /** Open the Overview board afterwards. Default true. */
  overview?: boolean;
}

/**
 * Focus mode: the calm window (SPEC §3.10). Entering remembers the person's own values for every
 * setting it touches (user level, in global state) and writes the calm ones; leaving puts theirs back,
 * removing a key they had never set. It never touches workspace settings, and it asks once before it
 * changes anything of theirs.
 */
export class FocusMode implements vscode.Disposable {
  constructor(private readonly memento: vscode.Memento) {
    void this.publish();
  }

  static register(context: vscode.ExtensionContext): FocusMode {
    const focus = new FocusMode(context.globalState);
    context.subscriptions.push(
      focus,
      vscode.commands.registerCommand('escurel.focusMode.enter', (o?: EnterOptions) =>
        focus.enter(o),
      ),
      vscode.commands.registerCommand('escurel.focusMode.exit', () => focus.exit()),
      vscode.commands.registerCommand('escurel.focusMode.toggle', () =>
        focus.isOn() ? focus.exit() : focus.enter(),
      ),
    );
    return focus;
  }

  isOn(): boolean {
    return Object.keys(this.memento.get<SavedSettings>(SAVED_KEY) ?? {}).length > 0;
  }

  async enter(options: EnterOptions = {}): Promise<boolean> {
    if (!options.silent && !this.isOn()) {
      const pick = await vscode.window.showInformationMessage(
        'Switch this window to the calm focus view? Menus, status bar and the developer chrome are hidden; you can switch back any time.',
        'Switch',
        'Not now',
      );
      if (pick !== 'Switch') return false;
    }
    const config = vscode.workspace.getConfiguration();
    const current: Record<string, unknown> = {};
    for (const key of Object.keys(FOCUS_SETTINGS)) {
      current[key] = config.inspect(key)?.globalValue;
    }
    const plan = planEnter(current, this.memento.get<SavedSettings>(SAVED_KEY));
    // Remember BEFORE writing: a crash half way must still be able to put things back.
    await this.memento.update(SAVED_KEY, plan.saved);
    await this.write(plan.writes);
    await this.publish();
    if (options.overview !== false) {
      await vscode.commands.executeCommand('escurel.openOverview');
    }
    return true;
  }

  async exit(): Promise<void> {
    const saved = this.memento.get<SavedSettings>(SAVED_KEY);
    await this.write(planExit(saved));
    await this.memento.update(SAVED_KEY, undefined);
    await this.publish();
  }

  private async write(values: Record<string, unknown>): Promise<void> {
    const config = vscode.workspace.getConfiguration();
    for (const [key, value] of Object.entries(values)) {
      try {
        await config.update(key, value, vscode.ConfigurationTarget.Global);
      } catch (err) {
        // A setting this VS Code does not know (an older one) must not stop the rest.
        log().warn(`escurel: focus mode could not set ${key}: ${(err as Error).message}`);
      }
    }
  }

  private async publish(): Promise<void> {
    await vscode.commands.executeCommand('setContext', CONTEXT_KEY, this.isOn());
  }

  dispose(): void {}
}
