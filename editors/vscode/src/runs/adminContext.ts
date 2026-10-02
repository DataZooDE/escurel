import * as vscode from 'vscode';
import type { Services } from '../services';

let currentCanAdmin = true;

/** The last value published to the VS Code context key. */
export function adminContextValue(): boolean {
  return currentCanAdmin;
}

function publish(value: boolean): void {
  currentCanAdmin = value;
  void vscode.commands.executeCommand('setContext', 'escurel.canAdmin', value);
}

/** Unknown stays enabled so the gateway can make the decision. */
export function registerAdminContext(context: vscode.ExtensionContext, services: Services): void {
  let generation = 0;
  const refresh = (): void => {
    const current = ++generation;
    void services.admin.get().then((state) => {
      if (current === generation) publish(state !== 'not-admin');
    });
  };
  context.subscriptions.push(services.admin.onDidChange(refresh));
  publish(true);
  refresh();
}
