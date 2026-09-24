import * as vscode from 'vscode';

let channel: vscode.LogOutputChannel | undefined;

/** The one output channel; every host-side component logs through it. */
export function log(): vscode.LogOutputChannel {
  channel ??= vscode.window.createOutputChannel('escurel', { log: true });
  return channel;
}
