import * as vscode from 'vscode';
import { log } from '../log';
import { FOCUS_SETTINGS, planEnter, planExit, type SavedSettings } from './focusSettings';
import { BUILTIN_PROFILE, chooseProfile, type FocusProfileRegistry } from './focusProfiles';

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
  /** Set synchronously, before entering can yield to a connection callback. */
  silentBootstrap = false;
  /**
   * What was remembered, held here as the truth and written through to the memento. VS Code's global
   * state may answer a `get` with the value from before a just-awaited `update`, and a window that
   * forgets it is calm cannot put the person's settings back.
   */
  private saved: SavedSettings | undefined;
  private async remember(saved: SavedSettings | undefined): Promise<void> {
    this.saved = saved;
    await this.memento.update(SAVED_KEY, saved);
  }
  /** Enter, switch and exit take turns: a settings write yields, and events arrive while it does. */
  private turn: Promise<unknown> = Promise.resolve();
  private serial<T>(work: () => Promise<T>): Promise<T> {
    const run = this.turn.then(work, work);
    this.turn = run.catch(() => undefined);
    return run;
  }
  constructor(
    private readonly memento: vscode.Memento,
    private readonly profiles?: {
      registry: FocusProfileRegistry;
      /** `escurel.focus.profile`: the profile the user chose, or empty. */
      configured: () => string | undefined;
    },
  ) {
    this.saved = memento.get<SavedSettings>(SAVED_KEY);
    void this.publish();
  }

  /** The settings the active profile wants: a companion's when it is the one chosen, else Escurel's own. */
  private activeSettings(): Readonly<Record<string, unknown>> {
    if (!this.profiles) return FOCUS_SETTINGS;
    const id = chooseProfile(this.profiles.configured(), this.profiles.registry.ids());
    if (id === BUILTIN_PROFILE) return FOCUS_SETTINGS;
    return this.profiles.registry.get(id)?.settings ?? FOCUS_SETTINGS;
  }

  static register(context: vscode.ExtensionContext, registry?: FocusProfileRegistry): FocusMode {
    const configured = (): string | undefined =>
      vscode.workspace.getConfiguration('escurel').get<string>('focus.profile');
    const focus = new FocusMode(
      context.globalState,
      registry ? { registry, configured } : undefined,
    );
    if (registry) {
      // A profile appeared, went away or was chosen while the window is calm: apply the active one now.
      context.subscriptions.push(
        registry.onDidChange(() => void focus.reapply()),
        vscode.workspace.onDidChangeConfiguration((e) => {
          if (e.affectsConfiguration('escurel.focus.profile')) void focus.reapply();
        }),
      );
    }
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
    return Object.keys(this.saved ?? {}).length > 0;
  }

  async enter(options: EnterOptions = {}): Promise<boolean> {
    if (options.silent) this.silentBootstrap = true;
    if (!options.silent && !this.isOn()) {
      const pick = await vscode.window.showInformationMessage(
        'Switch this window to the calm focus view? Menus, status bar and the developer chrome are hidden; you can switch back any time.',
        'Switch',
        'Not now',
      );
      if (pick !== 'Switch') return false;
    }
    await this.serial(() => this.apply());
    if (options.overview !== false) {
      await vscode.commands.executeCommand('escurel.openOverview');
    }
    return true;
  }

  /** Writes the active profile; what the previous one changed and this one does not is given back. */
  private async apply(): Promise<void> {
    const settings = this.activeSettings();
    const config = vscode.workspace.getConfiguration();
    const current: Record<string, unknown> = {};
    for (const key of Object.keys(settings)) {
      current[key] = config.inspect(key)?.globalValue;
    }
    const plan = planEnter(current, this.saved, settings);
    // Remember BEFORE writing: a crash half way must still be able to put things back.
    await this.remember(plan.saved);
    await this.write(plan.writes);
    await this.publish();
  }

  /** The active profile changed (registered, removed or chosen): while the window is calm, apply it. */
  async reapply(): Promise<void> {
    await this.serial(async () => {
      if (this.isOn()) await this.apply();
    });
  }

  exit(): Promise<void> {
    return this.serial(async () => {
      const saved = this.saved;
      await this.write(planExit(saved));
      await this.remember(undefined);
      await this.publish();
    });
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
