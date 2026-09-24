import * as vscode from 'vscode';
import { log } from './log';
import { Services } from './services';
import { EscurelAuthProvider } from './auth/provider';
import { EscurelError } from './client';

export function activate(context: vscode.ExtensionContext): void {
  const services = new Services(context);
  context.subscriptions.push(services);

  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.signIn', async () => {
      if (!EscurelAuthProvider.configured()) {
        void vscode.window.showInformationMessage(
          'escurel: no OIDC issuer is configured (escurel.auth.issuer), so this gateway is used without a token.',
        );
        return;
      }
      try {
        const s = await vscode.authentication.getSession('escurel', [], { createIfNone: true });
        void vscode.window.showInformationMessage(`escurel: signed in as ${s.account.label}`);
      } catch (e) {
        void vscode.window.showErrorMessage(`escurel: sign-in failed — ${(e as Error).message}`);
      }
    }),
    vscode.commands.registerCommand('escurel.signOut', async () => {
      await services.auth.removeSession();
      void vscode.window.showInformationMessage('escurel: signed out');
    }),
    vscode.commands.registerCommand('escurel.refresh', () => services.onDidChangeEmit()),
  );
  log().info('escurel: activated');
}

export function deactivate(): void {}

/** Shown to the user for any failed gateway call: the kind first, then the message. */
export function describeError(e: unknown): string {
  if (e instanceof EscurelError) {
    switch (e.kind) {
      case 'unauthorized':
        return 'not signed in, or the token expired — run "Escurel: Sign In"';
      case 'forbidden':
        return `forbidden: ${e.message}`;
      case 'session_cap_reached':
        return 'the gateway has no free session slot right now; try again in a moment';
      default:
        return `${e.kind}: ${e.message}`;
    }
  }
  return e instanceof Error ? e.message : String(e);
}
