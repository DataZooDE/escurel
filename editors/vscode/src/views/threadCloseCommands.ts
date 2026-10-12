import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import {
  buildThreadCloseEvent,
  resolveThreadRoot,
  type ThreadCloseArgument,
  type ThreadCloseAction,
} from './closedThreads';

/** The confirm dialog for closing: hiding, not deleting — anyone can reopen. */
export const CLOSE_THREAD_MESSAGE = 'Hide this thread from your list? Anyone can reopen it.';

/** Where the commands read the outline's open thread and refresh the Closed group. */
export interface ThreadCloseDeps {
  getOutlineRoot: () => string | undefined;
  refreshClosed: () => void;
}

async function act(
  services: Services,
  deps: ThreadCloseDeps,
  action: ThreadCloseAction,
  arg: ThreadCloseArgument,
): Promise<void> {
  const rootEventId = resolveThreadRoot(arg, deps.getOutlineRoot());
  if (!rootEventId) {
    void vscode.window.showInformationMessage(
      `Select a thread in the Threads view, then use its menu to ${action} it.`,
    );
    return;
  }
  let reason: string | undefined;
  if (action === 'close') {
    const answer = await vscode.window.showWarningMessage(
      CLOSE_THREAD_MESSAGE,
      { modal: true },
      'Close thread',
    );
    if (answer !== 'Close thread') return;
    const input = await vscode.window.showInputBox({
      prompt: 'Reason (optional)',
      placeHolder: 'Why is this thread closed?',
    });
    // Esc backs out of the whole close; empty means no reason.
    if (input === undefined) return;
    reason = input.trim() || undefined;
  }
  try {
    await services.client.captureEvent(buildThreadCloseEvent(rootEventId, action, reason));
  } catch (error) {
    // Only HERE is a refusal real: the gateway did not take the request.
    void vscode.window.showErrorMessage(describeError(error));
    return;
  }
  deps.refreshClosed();
  // A routine outcome is one status-bar line, not a notification.
  vscode.window.setStatusBarMessage(
    action === 'close' ? 'Thread closed. Anyone can reopen it.' : 'Thread reopened.',
    8000,
  );
}

export function registerThreadCloseCommands(
  context: vscode.ExtensionContext,
  services: Services,
  deps: ThreadCloseDeps,
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.closeThread', (arg?: ThreadCloseArgument) =>
      act(services, deps, 'close', arg),
    ),
    vscode.commands.registerCommand('escurel.reopenThread', (arg?: ThreadCloseArgument) =>
      act(services, deps, 'reopen', arg),
    ),
  );
}
