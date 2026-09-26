import * as vscode from 'vscode';
import { log } from './log';
import { Services } from './services';
import { EscurelAuthProvider } from './auth/provider';
import { EscurelFileSystem } from './fs/provider';
import { registerSkillDiagnostics } from './skills/diagnostics';
import { WikilinkProvider } from './skills/links';
import { KnowledgeTree } from './views/knowledge';
import { InboxTree } from './views/inbox';
import { AwaitingTree } from './views/awaiting';
import { PageAsUiEditor } from './editors/pageAsUi';
import { openPage, resolveCommand, searchCommand } from './commands/search';
import { ReviewController } from './review';
import { LiveCoordinator } from './live';

/** What `activate` returns — the integration suite drives the extension through it. */
export interface EscurelApi {
  services: Services;
  knowledge: KnowledgeTree;
  inbox: InboxTree;
  awaiting: AwaitingTree;
  review: ReviewController;
  live: LiveCoordinator;
}

export function activate(context: vscode.ExtensionContext): EscurelApi {
  const services = new Services(context);
  context.subscriptions.push(services);
  registerSkillDiagnostics(context, () => services.client);
  WikilinkProvider.register(context);
  const knowledge = KnowledgeTree.register(context, () => services.client);
  // The Threads outline is declared in package.json so no later slice has to edit
  // the manifest; a declared view with no provider shows VS Code's own error, so
  // it gets a placeholder until that slice arrives.
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('escurel.threads', {
      getChildren: () => [
        { label: 'Threads arrive in M3', collapsibleState: vscode.TreeItemCollapsibleState.None },
      ],
      getTreeItem: (e: vscode.TreeItem) => e,
    }),
  );
  const inbox = InboxTree.register(context, () => services.client);
  const awaiting = AwaitingTree.register(context, () => services.client);
  EscurelFileSystem.register(
    context,
    () => services.client,
    () => awaiting.refresh(),
    () => services.subject(),
  );
  const review = ReviewController.register(
    context,
    () => services.client,
    () => awaiting.refresh(),
  );
  const live = LiveCoordinator.register(context, services, { inbox, awaiting });
  context.subscriptions.push(
    services.onDidChange(() => {
      knowledge.refresh();
      inbox.refresh();
      awaiting.refresh();
    }),
  );
  PageAsUiEditor.register(context, () => services.client, services.onDidChange);

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
      (arg?: string | vscode.Uri | { pageId?: string; body?: string | null }) => {
        const pageId =
          typeof arg === 'string'
            ? arg
            : arg instanceof vscode.Uri
              ? arg.path.replace(/^\//, 'markdown/')
              : arg?.pageId;
        if (pageId) return openPage(pageId, true);
        if (typeof arg === 'object' && arg && 'body' in arg && arg.body) {
          return vscode.workspace
            .openTextDocument({ content: arg.body, language: 'markdown' })
            .then((doc) => vscode.window.showTextDocument(doc, { preview: true }));
        }
        const uri = vscode.window.activeTextEditor?.document.uri;
        if (uri?.scheme === 'escurel') return vscode.window.showTextDocument(uri);
        return undefined;
      },
    ),
    vscode.commands.registerCommand('escurel.openThread', () =>
      vscode.window.showInformationMessage('escurel: the Thread view arrives in M3.'),
    ),
    vscode.commands.registerCommand('escurel.openRun', () =>
      vscode.window.showInformationMessage('escurel: run detail arrives in M3.'),
    ),
    vscode.commands.registerCommand('escurel.startSkill', () =>
      vscode.window.showInformationMessage(
        'escurel: starting a skill arrives with M4 (Runner and starting skills).',
      ),
    ),
  );
  log().info('escurel: activated');
  return { services, knowledge, inbox, awaiting, review, live };
}

export function deactivate(): void {}
