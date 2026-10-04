import * as vscode from 'vscode';

/**
 * Routine feedback (started, signed in, copied...): a message in the status bar that fades by
 * itself, not a toast that has to be dismissed and that covers the panel. Toasts stay for
 * failures and for things that need the person.
 */
export function quietly(message: string, ms = 5000): void {
  vscode.window.setStatusBarMessage(message, ms);
}
