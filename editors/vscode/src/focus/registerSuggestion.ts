import * as vscode from 'vscode';
import type { FocusMode } from './focusMode';
import type { Services } from '../services';
import type { LiveCoordinator } from '../live';
import { log } from '../log';
import { FocusSuggestion, SUGGESTED_KEY } from './focusSuggestion';

/** Listen to successful connections, not Services.onDidChange (which also fires offline). */
export function registerFocusSuggestion(
  context: vscode.ExtensionContext, focus: FocusMode, services: Services, live: LiveCoordinator,
): void {
  let signedIn = false;
  let disposed = false;
  const muted = () =>
    disposed || !vscode.workspace.isTrusted || vscode.env.uiKind === vscode.UIKind.Web ||
    !!vscode.workspace.workspaceFolders?.some(folder =>
      folder.uri.scheme !== 'file' && folder.uri.scheme !== 'vscode-remote');
  const suggestion = new FocusSuggestion({
    state: () => ({
      connected: signedIn && live.socketState === 'open' && !muted(),
      focusActive: focus.isOn(),
      alreadySuggested: context.globalState.get<boolean>(SUGGESTED_KEY) ?? false,
      // Bootstraps can connect before calling enter. Detect them before their async setup starts.
      silentBootstrap: focus.silentBootstrap ||
        !!vscode.extensions.getExtension('datazoo.escurel-demo-bootstrap') ||
        !!vscode.extensions.getExtension('datazoo.escurel-web-bootstrap'),
    }),
    remember: () => context.globalState.update(SUGGESTED_KEY, true),
    show: (message, ...buttons) => vscode.window.showInformationMessage(message, ...buttons),
    enter: () => vscode.commands.executeCommand('escurel.focusMode.enter', { silent: true }),
  });
  context.subscriptions.push(
    { dispose: () => { disposed = true; } },
    live.onDidConnect(async () => {
      const socket = live.activeSocket;
      try {
        signedIn = !!await services.subject();
        if (socket !== live.activeSocket || muted()) return;
        await suggestion.check();
      } catch (err) {
        log().warn(`escurel: focus suggestion failed: ${String(err)}`);
      }
    }),
  );
}
