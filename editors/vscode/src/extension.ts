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
import { RunController } from './runs/controller';
import { registerControlCommands } from './runs/controlCommands';
import { adminContextValue } from './runs/adminContext';
import { ThreadController } from './thread/controller';
import { buildInspectors } from './thread/inspector';
import { toThreadView } from './thread/threadModel';
import { ThreadsTree } from './views/threads';
import { expandableRows, type OutlineRow } from './views/threadsModel';
import { registerStartInTerminal } from './start/terminal';

/** What `activate` returns — the integration suite drives the extension through it. */
export interface EscurelApi {
  services: Services;
  knowledge: KnowledgeTree;
  inbox: InboxTree;
  awaiting: AwaitingTree;
  review: ReviewController;
  live: LiveCoordinator;
  threads: ThreadController;
  runs: RunController;
  threadsTree: ThreadsTree;
  /**
   * The value last published to the `escurel.canAdmin` context key, which greys the admin-only
   * controls. Read from THIS bundle: a test bundle importing the module would see its own copy.
   */
  canAdmin: () => boolean;
}

export function activate(context: vscode.ExtensionContext): EscurelApi {
  const services = new Services(context);
  registerControlCommands(context, services);
  context.subscriptions.push(services);
  registerStartInTerminal(context, services);
  registerSkillDiagnostics(context, () => services.client);
  WikilinkProvider.register(context);
  const knowledge = KnowledgeTree.register(context, () => services.client);
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
  const threadsTree = new ThreadsTree();
  const threadsView = vscode.window.createTreeView('escurel.threads', {
    treeDataProvider: threadsTree,
    showCollapseAll: true,
  });
  threadsView.message = threadsTree.message;
  context.subscriptions.push(threadsView);

  const threads = ThreadController.register(context, services, (loaded) =>
    buildInspectors(toThreadView(loaded), [...loaded.nodes.values()]),
  );

  // The outline follows the thread the user last looked at; closing that panel empties it.
  // A row can be rebuilt (a live reload, a collapse) between asking the tree to reveal it and
  // the tree resolving it, and `reveal` then rejects with 'Cannot resolve tree item'. Nothing
  // waits on a reveal, so that rejection was surfacing as an unhandled one.
  const revealQuietly = (
    row: OutlineRow,
    options: { expand?: number | boolean; select?: boolean; focus?: boolean },
  ): void => {
    void threadsView.reveal(row, options).then(undefined, () => undefined);
  };
  let outlineRoot: string | undefined;
  context.subscriptions.push(
    threads.onDidLoad(({ rootEventId, thread, collapsed }) => {
      if (thread) {
        outlineRoot = rootEventId;
        threadsTree.setThread(toThreadView(thread), collapsed);
        // A row that gains children while the thread is live stays collapsed, so a run that
        // arrives would be hidden until the user noticed and expanded it. Seen in a real
        // window: the canvas showed the new run and the outline did not. Every row the canvas
        // has NOT collapsed is held open; one the user collapsed there is left alone.
        for (const row of expandableRows(threadsTree.getChildren())) {
          revealQuietly(row, { expand: true, select: false, focus: false });
        }
      } else if (outlineRoot === rootEventId) {
        outlineRoot = undefined;
        threadsTree.setThread(undefined);
      }
      threadsView.message = threadsTree.message;
    }),
    // Canvas → outline: a card collapsed on the canvas is collapsed in the tree.
    threads.onDidCollapse(({ rootEventId, collapsed }) => {
      if (rootEventId === outlineRoot) threadsTree.setCollapsed(collapsed);
    }),
    // Canvas → outline: a card selected on the canvas is revealed in the tree.
    threads.onDidSelect(({ rootEventId, nodeId }) => {
      const row = rootEventId === outlineRoot ? threadsTree.rowFor(nodeId) : undefined;
      if (row) revealQuietly(row, { select: true, focus: false });
    }),
    // Outline → canvas: a row selected in the tree selects and scrolls to its card.
    threadsView.onDidChangeSelection((e) => {
      const row = e.selection[0];
      if (row && outlineRoot) threads.select(outlineRoot, row.id);
    }),
  );
  const runs = RunController.register(context, services);
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

    vscode.commands.registerCommand('escurel.startSkill', () =>
      vscode.window.showInformationMessage(
        'escurel: starting a skill arrives with M4 (Runner and starting skills).',
      ),
    ),
  );
  log().info('escurel: activated');
  return {
    services,
    knowledge,
    inbox,
    awaiting,
    review,
    live,
    threads,
    runs,
    threadsTree,
    canAdmin: adminContextValue,
  };
}

export function deactivate(): void {}
