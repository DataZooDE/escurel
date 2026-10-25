import * as vscode from 'vscode';
import { log } from '../log';
import { readConfig } from '../config';
import { createHelperCredentialBroker, type HelperBroker } from './helperCredential';

const CONSENT_KEY = 'escurel.api.helperCredentialConsent';
export const RESET_CONSENT_COMMAND = 'escurel.api.resetHelperCredentialConsent';

/**
 * Wires the helper-credential broker to the host: the user-level allowlist setting, a remembered consent
 * in global state, the modal question, and the gateway's `mint_agent_token`.
 */
export function registerHelperCredential(
  context: vscode.ExtensionContext,
  mint: (req: {
    skill: string;
    ttl_secs: number;
  }) => Promise<{ token: string; expires_at: string }>,
  gatewayUrl: () => string,
  hasBearer: () => Promise<boolean>,
): HelperBroker {
  const granted = (): string[] => {
    const v = context.globalState.get<unknown>(CONSENT_KEY);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
  };
  context.subscriptions.push(
    vscode.commands.registerCommand(RESET_CONSENT_COMMAND, async () => {
      await context.globalState.update(CONSENT_KEY, undefined);
      void vscode.window.showInformationMessage(
        'Escurel: extensions must ask again before they get a helper credential.',
      );
    }),
  );
  return createHelperCredentialBroker({
    authRequired: async () => readConfig().auth.issuer.trim() !== '' || (await hasBearer()),
    mint,
    // `inspect().globalValue`: only the user's own setting counts; a workspace file cannot add an extension.
    allowlist: () =>
      vscode.workspace.getConfiguration('escurel').inspect('api.helperCredentialExtensions')
        ?.globalValue,
    consents: {
      has: (id) => granted().includes(id),
      grant: (id) => Promise.resolve(context.globalState.update(CONSENT_KEY, [...granted(), id])),
    },
    confirm: async (message) => {
      const yes = 'Allow';
      return (await vscode.window.showWarningMessage(message, { modal: true }, yes)) === yes;
    },
    isInstalled: (id) => vscode.extensions.getExtension(id) !== undefined,
    gatewayUrl,
    warn: (m) => log().warn(m),
  });
}
