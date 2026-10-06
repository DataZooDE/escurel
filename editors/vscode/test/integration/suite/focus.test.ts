// Focus mode (SPEC §3.10): the calm window on and off, in a real VS Code, against the real settings
// store. A person's own settings must come back exactly, including the ones they never set.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';

const config = () => vscode.workspace.getConfiguration();
const user = (key: string) => config().inspect(key)?.globalValue;

suite('Focus mode', () => {
  suiteSetup(async () => {
    await vscode.extensions.getExtension('datazoo.escurel')!.activate();
  });

  teardown(async () => {
    await vscode.commands.executeCommand('escurel.focusMode.exit');
    await config().update('window.zoomLevel', undefined, vscode.ConfigurationTarget.Global);
  });

  test('enter hides the chrome; exit puts the person’s own values back and removes the rest', async () => {
    await config().update('window.zoomLevel', 2, vscode.ConfigurationTarget.Global);
    await config().update('breadcrumbs.enabled', true, vscode.ConfigurationTarget.Global);

    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: false,
    });
    assert.equal(user('workbench.statusBar.visible'), false);
    assert.equal(user('window.menuBarVisibility'), 'hidden');
    assert.equal(user('window.title'), 'Escurel');
    assert.equal(user('window.zoomLevel'), 1);
    assert.equal(user('breadcrumbs.enabled'), false);

    // A second enter must not take the focus values for the person's own.
    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: false,
    });

    await vscode.commands.executeCommand('escurel.focusMode.exit');
    assert.equal(user('window.zoomLevel'), 2, 'their zoom is back');
    assert.equal(user('breadcrumbs.enabled'), true, 'their explicit true is back');
    assert.equal(user('workbench.statusBar.visible'), undefined, 'never set: removed again');
    assert.equal(user('window.title'), undefined);
  });
});
