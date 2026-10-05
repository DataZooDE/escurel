import * as vscode from 'vscode';

/** The settings the extension reads (SPEC §1: one gateway, one tenant). */
export interface EscurelConfig {
  gatewayUrl: string;
  evolveEndpoint: string;
  auth: { issuer: string; clientId: string; scopes: string[] };
  shellHarness: string;
  harness: string;
}

export function readConfig(): EscurelConfig {
  const c = vscode.workspace.getConfiguration('escurel');
  return {
    gatewayUrl: (c.get<string>('gatewayUrl') ?? '').replace(/\/+$/, ''),
    evolveEndpoint: (c.get<string>('evolveEndpoint') ?? '').replace(/\/+$/, ''),
    auth: {
      issuer: c.get<string>('auth.issuer') ?? '',
      clientId: c.get<string>('auth.clientId') ?? '',
      scopes: c.get<string[]>('auth.scopes') ?? ['openid', 'profile', 'offline_access'],
    },
    shellHarness: c.get<string>('shellHarness') ?? 'claude',
    harness: c.get<string>('harness') ?? '',
  };
}

export function onConfigChange(listener: () => void): vscode.Disposable {
  return vscode.workspace.onDidChangeConfiguration((e) => {
    if (e.affectsConfiguration('escurel')) listener();
  });
}
