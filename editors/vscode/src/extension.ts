import { quietly } from './shared/quiet';
import { exposedApi } from './shared/apiExposure';
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
import { SkillPageEditor } from './editors/skillPage';
import { openPage, resolveCommand, searchCommand } from './commands/search';
import { ReviewController } from './review';
import { LiveCoordinator } from './live';
import { RunController } from './runs/controller';
import { registerControlCommands } from './runs/controlCommands';
import { adminContextValue } from './runs/adminContext';
import { ThreadController } from './thread/controller';
import { DetailsViewProvider } from './thread/detailsView';
import { buildInspectors } from './thread/inspector';
import { toThreadView } from './thread/threadModel';
import { ThreadsTree } from './views/threads';
import { expandableRows, type OutlineRow } from './views/threadsModel';
import { registerStartInTerminal } from './start/terminal';
import { registerOpenOriginal } from './commands/openOriginal';
import { registerStartSkill } from './start/startSkill';
import { registerProposeWriteBack } from './editors/proposeWriteBack';
import { registerApprovePlan, setApprovalConfirm } from './start/approvePlan';
import { registerNodeCommands } from './commands/nodeCommands';
import { explainText } from './shared/explain';
import { registerRunnerView, type RunnerTree } from './views/runner';
import { registerImportEvolveProblem } from './evolve/importProblem';
import { registerPrepareEvolveTrainingSource } from './evolve/prepareSource';
import { registerEvolveHoldout } from './evolve/registerHoldout';
import { registerEvolveHoldoutCsv } from './evolve/registerHoldoutCsv';
import { registerPrepareEvolveTrainingCsv } from './evolve/prepareCsv';
import { ScenariosTree } from './views/scenarios';

const EXPLAIN_SCHEME = 'escurel-explain';

/** What `activate` returns — the integration suite drives the extension through it. */
export interface EscurelApi {
  services: Services;
  knowledge: KnowledgeTree;
  inbox: InboxTree;
  awaiting: AwaitingTree;
  review: ReviewController;
  live: LiveCoordinator;
  threads: ThreadController;
  /** The bottom-panel details view of the node selected in a thread. */
  details: DetailsViewProvider;
  runs: RunController;
  threadsTree: ThreadsTree;
  /**
   * The value last published to the `escurel.canAdmin` context key, which greys the admin-only
   * controls. Read from THIS bundle: a test bundle importing the module would see its own copy.
   */
  canAdmin: () => boolean;
  runner: RunnerTree;
  /** Test seam: replaces the modal that confirms a plan approval (a modal blocks a headless window). */
  setApprovalConfirm: typeof setApprovalConfirm;
}

export function activate(context: vscode.ExtensionContext): EscurelApi | undefined {
  const services = new Services(context);
  registerControlCommands(context, services);
  registerNodeCommands(context, services);
  context.subscriptions.push(services);
  registerStartInTerminal(context, services);
  registerImportEvolveProblem(context, services);
  registerPrepareEvolveTrainingSource(context, services);
  registerPrepareEvolveTrainingCsv(context, services);
  registerEvolveHoldout(context, services);
  registerEvolveHoldoutCsv(context, services);
  registerOpenOriginal(context, services);
  registerSkillDiagnostics(context, () => services.client);
  WikilinkProvider.register(context);
  const knowledge = KnowledgeTree.register(context, () => services.client);
  const inbox = InboxTree.register(context, () => services.client);
  const awaiting = AwaitingTree.register(context, () => services.client);
  const runner = registerRunnerView(context, services);
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
  const scenarios = ScenariosTree.register(context, () => services.client, services);
  // A comparison click (or any other Evolve event) is the moment the Scenarios view may have moved.
  context.subscriptions.push(
    live.onDidReceiveEvent((event) => {
      if (event.label_skill.startsWith('evolve')) scenarios.refresh();
    }),
  );
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
  const details = DetailsViewProvider.register(context, threads);
  const runs = RunController.register(context, services);
  context.subscriptions.push(
    services.onDidChange(() => {
      knowledge.refresh();
      inbox.refresh();
      awaiting.refresh();
    }),
  );
  PageAsUiEditor.register(context, () => services.client, services.onDidChange);
  SkillPageEditor.register(context, () => services.client, services.onDidChange);

  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.signIn', async () => {
      if (!EscurelAuthProvider.configured()) {
        void vscode.window.showInformationMessage(
          'No OIDC issuer is configured (escurel.auth.issuer), so this gateway is used without a token.',
        );
        return;
      }
      try {
        const s = await vscode.authentication.getSession('escurel', [], { createIfNone: true });
        quietly(`Signed in as ${s.account.label}`);
      } catch (e) {
        log().warn(`escurel: sign-in failed: ${(e as Error).message}`);
        void vscode.window.showErrorMessage(
          'Sign-in did not work. Check the gateway address and the sign-in provider, then try again. Details are in the Escurel output log.',
        );
      }
    }),
    vscode.commands.registerCommand('escurel.signOut', async () => {
      await services.auth.removeSession();
      quietly('Signed out');
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

    vscode.commands.registerCommand('escurel.showDetails', () =>
      vscode.commands.executeCommand('escurel.details.focus'),
    ),
    vscode.commands.registerCommand('escurel.showRunner', () =>
      vscode.commands.executeCommand('escurel.runner.focus'),
    ),
    vscode.commands.registerCommand('escurel.focusCanvas', () => {
      const shown = details.current();
      if (shown) threads.focusCanvas(shown.rootEventId);
      else
        void vscode.window.showInformationMessage('Open a thread first, then select a node in it.');
    }),
    // 'Explain this view': how events, skills, runs, changesets and instances connect, in plain words.
    // A named document, so the preview tab is titled for what it is and not 'Preview Untitled-1'.
    vscode.workspace.registerTextDocumentContentProvider(EXPLAIN_SCHEME, {
      provideTextDocumentContent: () => explainText(),
    }),
    vscode.commands.registerCommand('escurel.explainView', async () => {
      await vscode.commands.executeCommand(
        'markdown.showPreviewToSide',
        vscode.Uri.from({ scheme: EXPLAIN_SCHEME, path: '/How things connect in Escurel.md' }),
      );
    }),
    vscode.commands.registerCommand('escurel.focusRuns', () =>
      vscode.commands.executeCommand('escurel.runner.focus'),
    ),
    vscode.commands.registerCommand('escurel.focusAwaiting', () =>
      vscode.commands.executeCommand('escurel.awaiting.focus'),
    ),
    vscode.commands.registerCommand('escurel.focusInbox', () =>
      vscode.commands.executeCommand('escurel.inbox.focus'),
    ),
    vscode.commands.registerCommand('escurel.focusKnowledge', () =>
      vscode.commands.executeCommand('escurel.knowledge.focus'),
    ),

    registerStartSkill(context, services),
    registerProposeWriteBack(services),
    registerApprovePlan(context, services),
  );
  log().info('escurel: activated');
  // Other extensions can read an extension's `exports`, and this object holds the token store. A
  // production install hands out nothing; the test, e2e and demo harnesses (Test / Development mode)
  // get the API they drive the extension through.
  return exposedApi(context.extensionMode === vscode.ExtensionMode.Production, {
    services,
    knowledge,
    inbox,
    awaiting,
    review,
    live,
    threads,
    details,
    runs,
    threadsTree,
    canAdmin: adminContextValue,
    runner,
    setApprovalConfirm,
  });
}

export function deactivate(): void {}
