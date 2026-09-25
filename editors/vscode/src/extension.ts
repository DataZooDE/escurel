import * as vscode from 'vscode';
import { log } from './log';
import { Services } from './services';
import { EscurelAuthProvider } from './auth/provider';
import { EscurelFileSystem } from './fs/provider';
import { registerSkillDiagnostics } from './skills/diagnostics';
import { WikilinkProvider } from './skills/links';
import { KnowledgeTree } from './views/knowledge';
import { openPage, resolveCommand, searchCommand } from './commands/search';

export function activate(context: vscode.ExtensionContext): void {
  const services = new Services(context);
  context.subscriptions.push(services);
  EscurelFileSystem.register(context, () => services.client);
  registerSkillDiagnostics(context, () => services.client);
  WikilinkProvider.register(context);
  const knowledge = KnowledgeTree.register(context, () => services.client);
  context.subscriptions.push(services.onDidChange(() => knowledge.refresh()));

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
    vscode.commands.registerCommand('escurel.search', () => searchCommand(() => services.client)),
    vscode.commands.registerCommand('escurel.resolve', (link?: string) =>
      resolveCommand(() => services.client, link),
    ),
    vscode.commands.registerCommand('escurel.openPage', (pageId: string) => openPage(pageId)),
    vscode.commands.registerCommand('escurel.openInstance', (arg: string | { pageId: string }) =>
      openPage(typeof arg === 'string' ? arg : arg.pageId),
    ),
    vscode.commands.registerCommand(
      'escurel.viewSkill',
      (arg: string | { skill: { id: string } } | { skill: string }) => {
        const id =
          typeof arg === 'string' ? arg : typeof arg.skill === 'string' ? arg.skill : arg.skill.id;
        return openPage(`markdown/skills/${id}.md`);
      },
    ),
    vscode.commands.registerCommand(
      'escurel.showRaw',
      (arg?: string | vscode.Uri | { pageId: string }) => {
        const pageId =
          typeof arg === 'string'
            ? arg
            : arg instanceof vscode.Uri
              ? arg.path.replace(/^\//, 'markdown/')
              : arg?.pageId;
        if (pageId) return openPage(pageId, true);
        const uri = vscode.window.activeTextEditor?.document.uri;
        if (uri?.scheme === 'escurel') return vscode.window.showTextDocument(uri);
        return undefined;
      },
    ),
    vscode.commands.registerCommand('escurel.startSkill', () =>
      vscode.window.showInformationMessage(
        'escurel: starting a skill arrives with M4 (Runner and starting skills).',
      ),
    ),
  );
  log().info('escurel: activated');
}

export function deactivate(): void {}
