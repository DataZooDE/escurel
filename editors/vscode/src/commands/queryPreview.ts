import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import { pageIdFromPath } from '../fs/read';
import {
  buildRuns,
  paramSpecs,
  queryPreviewMarkdown,
  type PreviewRun,
} from '../shared/queryPreview';

const SCHEME = 'escurel-query-preview';

/**
 * "Preview with parameters": a query page is a method a person can read, change and review. This runs it
 * for values they type (a comma list compares several at once) and shows the rows in VS Code's preview,
 * so the effect of a parameter is visible before anyone changes the page. Read-only: nothing is saved.
 */
export function registerQueryPreview(context: vscode.ExtensionContext, services: Services): void {
  const docs = new Map<string, string>();
  const changed = new vscode.EventEmitter<vscode.Uri>();
  context.subscriptions.push(
    changed,
    vscode.workspace.registerTextDocumentContentProvider(SCHEME, {
      onDidChange: changed.event,
      provideTextDocumentContent: (uri) => docs.get(uri.toString()) ?? '',
    }),
    vscode.commands.registerCommand(
      'escurel.previewQuery',
      async (arg?: string | { pageId?: string }) => {
        const pageId = (typeof arg === 'string' ? arg : arg?.pageId) ?? activePageId();
        if (!pageId || !pageId.includes('/instances/query/')) {
          void vscode.window.showInformationMessage(
            'Open a query page (Knowledge > query), then choose "Preview with parameters".',
          );
          return;
        }
        const id = pageId.split('/').at(-1)!.replace(/\.md$/, '');
        try {
          const e = await services.client.expand({ page_id: pageId });
          if (!e.page) throw new Error('This page does not exist, or you may not read it.');
          const fm = e.frontmatter ?? {};
          const specs = paramSpecs(fm);
          const memory = context.workspaceState.get<Record<string, string>>(`query:${id}`) ?? {};
          const answers: Record<string, string> = {};
          for (const s of specs) {
            const v = await vscode.window.showInputBox({
              title: `Preview ${id}`,
              prompt: `${s.name.replaceAll('_', ' ')} (${s.type}${s.required ? ', required' : ''}). A comma list, like 0.95, 0.98, 0.99, compares them side by side.`,
              value: memory[s.name] ?? '',
              ignoreFocusOut: true,
              validateInput: (t) =>
                s.required && t.trim() === '' ? `${s.name} is required` : undefined,
            });
            if (v === undefined) return;
            answers[s.name] = v;
          }
          await context.workspaceState.update(`query:${id}`, answers);
          const runs: PreviewRun[] = [];
          for (const params of buildRuns(specs, answers)) {
            try {
              const r = await services.client.queryInstance({
                ref: id,
                params: params as Record<string, string | number | boolean>,
              });
              runs.push({ params, rows: r.rows ?? [] });
            } catch (err) {
              runs.push({ params, rows: [], error: describeError(err) });
            }
          }
          const title = typeof fm.title === 'string' ? fm.title : id;
          const uri = vscode.Uri.from({ scheme: SCHEME, path: `/${title} (preview).md` });
          docs.set(
            uri.toString(),
            queryPreviewMarkdown({
              title,
              id,
              description: typeof fm.description === 'string' ? fm.description : '',
              runs,
            }),
          );
          changed.fire(uri);
          await vscode.commands.executeCommand('markdown.showPreviewToSide', uri);
        } catch (err) {
          void vscode.window.showErrorMessage(`Could not preview ${id}: ${describeError(err)}`);
        }
      },
    ),
  );
}

/** The page of the editor in front, when it is an escurel instance. */
function activePageId(): string | undefined {
  const input = vscode.window.tabGroups.activeTabGroup.activeTab?.input as
    { uri?: vscode.Uri } | undefined;
  const uri = input?.uri;
  return uri?.scheme === 'escurel' ? pageIdFromPath(uri.path)?.pageId : undefined;
}
