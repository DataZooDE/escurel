// Focus mode (SPEC §3.10): the calm window on and off, in a real VS Code, against the real settings
// store. A person's own settings must come back exactly, including the ones they never set.
import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';

const config = () => vscode.workspace.getConfiguration();
const user = (key: string) => config().inspect(key)?.globalValue;

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

suite('Focus mode', () => {
  let api: EscurelApi;
  suiteSetup(async () => {
    api = (await vscode.extensions.getExtension('datazoo.escurel')!.activate()) as EscurelApi;
  });

  teardown(async () => {
    await vscode.commands.executeCommand('escurel.focusMode.exit');
    await config().update('window.title', undefined, vscode.ConfigurationTarget.Global);
    await config().update('breadcrumbs.enabled', undefined, vscode.ConfigurationTarget.Global);
  });

  test('enter hides the chrome; exit puts the person’s own values back and removes the rest', async () => {
    await config().update('window.title', 'My window', vscode.ConfigurationTarget.Global);
    await config().update('breadcrumbs.enabled', true, vscode.ConfigurationTarget.Global);

    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: false,
    });
    assert.equal(user('workbench.statusBar.visible'), false);
    assert.equal(user('window.menuBarVisibility'), 'hidden');
    assert.equal(user('window.title'), 'Escurel');
    assert.equal(user('breadcrumbs.enabled'), false);

    // A second enter must not take the focus values for the person's own.
    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: false,
    });

    await vscode.commands.executeCommand('escurel.focusMode.exit');
    assert.equal(user('window.title'), 'My window', 'their title is back');
    assert.equal(user('breadcrumbs.enabled'), true, 'their explicit true is back');
    assert.equal(user('workbench.statusBar.visible'), undefined, 'never set: removed again');
  });

  test('the overview board offers keys, and the host opens only what it offered', async () => {
    await vscode.commands.executeCommand('escurel.openOverview');
    const end = Date.now() + 30_000;
    while (api.overview.offered.size === 0 && Date.now() < end) await wait(200);
    assert.ok(api.overview.offered.size > 0, 'the board read the corpus and offered lines');
    const [key] = [...api.overview.offered.keys()];

    // Forged messages: a key it never offered, a path, a tile that is not one, a prototype name.
    assert.equal(await api.overview.onMessage({ type: 'open', key: 'decisions:999' }), false);
    assert.equal(await api.overview.onMessage({ type: 'open', key: '../../etc/passwd' }), false);
    assert.equal(await api.overview.onMessage({ type: 'open', key: 42 }), false);
    assert.equal(await api.overview.onMessage({ type: 'open-tile', tile: '__proto__' }), false);
    assert.equal(await api.overview.onMessage({ type: 'open-tile', tile: 'constructor' }), false);
    assert.equal(await api.overview.onMessage('open'), false);
    assert.equal(await api.overview.onMessage({ type: 'run-anything' }), false);

    assert.equal(await api.overview.onMessage({ type: 'open', key }), true);
  });

  test('the board’s own switch toggles focus mode both ways', async () => {
    assert.equal(api.focus.isOn(), false);
    await api.overview.onMessage({ type: 'toggle-focus' });
    assert.equal(api.focus.isOn(), true);
    assert.equal(user('workbench.statusBar.visible'), false);
    await api.overview.onMessage({ type: 'toggle-focus' });
    assert.equal(api.focus.isOn(), false);
    assert.equal(user('workbench.statusBar.visible'), undefined);
  });
});
