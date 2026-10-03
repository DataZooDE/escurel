import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import { originalFileName } from './originalFile';

/**
 * `escurel.openOriginal`: the retained original file of a `document` page, written to the extension's
 * storage and opened with the system's own application (a PDF viewer, Word). The page id comes from the
 * host's own page model (see `resolvePageMessage`), never from a webview.
 */
export function registerOpenOriginal(context: vscode.ExtensionContext, services: Services): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('escurel.openOriginal', async (pageId?: unknown) => {
      if (typeof pageId !== 'string' || pageId === '') return;
      try {
        const { blob } = await services.client.fetchBlob(pageId);
        if (!blob) {
          void vscode.window.showInformationMessage('The original file is not available.');
          return;
        }
        const dir = vscode.Uri.joinPath(context.globalStorageUri, 'originals');
        await vscode.workspace.fs.createDirectory(dir);
        const file = vscode.Uri.joinPath(dir, originalFileName(pageId, blob.content_type));
        await vscode.workspace.fs.writeFile(file, Buffer.from(blob.bytes_base64, 'base64'));
        await vscode.env.openExternal(file);
      } catch (error) {
        void vscode.window.showErrorMessage(`Could not open the original: ${describeError(error)}`);
      }
    }),
  );
}
