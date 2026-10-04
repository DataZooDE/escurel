// The retained original of a document page is UNTRUSTED bytes written to the extension's storage. They
// must not pile up there: a session's files are removed when the extension starts the next time.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import { clearOriginals } from '../../../src/commands/openOriginal';

suite('originals written for the system application do not accumulate', () => {
  test('clearOriginals removes everything in the originals folder and tolerates it not existing', async () => {
    const root = vscode.Uri.joinPath(
      vscode.Uri.file(process.env.TMPDIR ?? '/tmp'),
      `escurel-originals-${process.pid}-${Date.now()}`,
    );
    const dir = vscode.Uri.joinPath(root, 'originals');
    await vscode.workspace.fs.createDirectory(dir);
    await vscode.workspace.fs.writeFile(
      vscode.Uri.joinPath(dir, 'a-0123456789ab.pdf'),
      Buffer.from('x'),
    );
    await vscode.workspace.fs.writeFile(
      vscode.Uri.joinPath(dir, 'b-0123456789ab.txt'),
      Buffer.from('y'),
    );

    await clearOriginals(root);
    // The folder is gone.
    await assert.rejects(async () => {
      await vscode.workspace.fs.stat(dir);
    });

    await clearOriginals(root); // nothing to remove: not an error
    await vscode.workspace.fs.delete(root, { recursive: true, useTrash: false });
  });
});
