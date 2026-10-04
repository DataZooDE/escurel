import * as vscode from 'vscode';
import { noticeActions, type NoticeTarget } from '../shared/notice';

export type NoticeLevel = 'info' | 'warning' | 'error';

/**
 * Show a notification that offers to open what it names (a page, a run, a thread, a skill). Every
 * notification that names something goes through here, so the buttons are the same everywhere. The
 * returned promise settles when the notice is dismissed or a button ran.
 */
export async function notify(
  level: NoticeLevel,
  message: string,
  targets: NoticeTarget[] = [],
): Promise<void> {
  const actions = noticeActions(targets);
  const show =
    level === 'error'
      ? vscode.window.showErrorMessage
      : level === 'warning'
        ? vscode.window.showWarningMessage
        : vscode.window.showInformationMessage;
  const choice = await show(message, ...actions.map((a) => a.label));
  const picked = actions.find((a) => a.label === choice);
  if (picked) await vscode.commands.executeCommand(picked.command, ...picked.args);
}
