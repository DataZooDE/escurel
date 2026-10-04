import * as vscode from 'vscode';
import type { Services } from '../services';
import { describeError } from '../errors';
import { planOriginal } from './originalFile';

/**
 * `escurel.openOriginal`: the retained original file of a `document` page, written to the extension's
 * storage and opened with the system's own application (a PDF viewer, Word). The page id comes from the
 * host's own page model (see `resolvePageMessage`), never from a webview.
 */
/**
 * Remove the originals a previous session wrote. They are untrusted uploads handed to another
 * application, and there is no reason to keep them: the folder is emptied when the extension starts.
 */
export async function clearOriginals(storageUri: vscode.Uri): Promise<void> {
  try {
    await vscode.workspace.fs.delete(vscode.Uri.joinPath(storageUri, 'originals'), {
      recursive: true,
      useTrash: false,
    });
  } catch {
    // Not there (first run) or not removable: nothing worth stopping for.
  }
}

export function registerOpenOriginal(context: vscode.ExtensionContext, services: Services): void {
  void clearOriginals(context.globalStorageUri);
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
        const plan = planOriginal(services.gatewayUrl, pageId, blob.content_type);
        const file = vscode.Uri.joinPath(dir, plan.fileName);
        await vscode.workspace.fs.writeFile(file, Buffer.from(blob.bytes_base64, 'base64'));
        if (plan.handling === 'reveal') {
          // Active or unknown content is never handed to an application.
          void vscode.window.showInformationMessage(
            'This file type is not opened from here, because it could run code. It is saved as a plain file.',
          );
          await vscode.commands.executeCommand('revealFileInOS', file);
          return;
        }
        if (plan.handling === 'confirm') {
          const answer = await vscode.window.showWarningMessage(
            'Open this document in your system application? Only open files you trust.',
            { modal: true },
            'Open',
          );
          if (answer !== 'Open') return;
        }
        await vscode.env.openExternal(file);
      } catch (error) {
        void vscode.window.showErrorMessage(`Could not open the original: ${describeError(error)}`);
      }
    }),
  );
}
