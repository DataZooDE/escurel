import * as vscode from 'vscode';
import { log } from './log';
import { readConfig } from './config';

export function activate(context: vscode.ExtensionContext): void {
  const cfg = readConfig();
  log().info(`escurel: activated against ${cfg.gatewayUrl}`);
  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.refresh', () => {
      log().info('escurel.refresh: nothing to refresh yet (M1 scaffold)');
    }),
  );
}

export function deactivate(): void {}
