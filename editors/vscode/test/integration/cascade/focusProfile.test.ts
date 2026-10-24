import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelExtensionApi } from '../../../src/api/types';
import { activate } from './support';

const config = () => vscode.workspace.getConfiguration();
const user = (key: string) => config().inspect(key)?.globalValue;

// Focus profiles in a real host, against the real settings store: two companions offer a profile, the
// toggle applies the chosen one, and the person's own values come back whichever way it was switched.
suite('focus profiles', () => {
  let api: EscurelExtensionApi;
  const subs: vscode.Disposable[] = [];

  suiteSetup(async function () {
    this.timeout(120_000);
    api = (await activate()) as unknown as EscurelExtensionApi;
  });

  teardown(async () => {
    await vscode.commands.executeCommand('escurel.focusMode.exit');
    for (const d of subs.splice(0)) d.dispose();
    for (const key of ['window.title', 'breadcrumbs.enabled', 'editor.minimap.enabled']) {
      await config().update(key, undefined, vscode.ConfigurationTarget.Global);
    }
    await config().update('escurel.focus.profile', undefined, vscode.ConfigurationTarget.Global);
  });

  test('the chosen profile is applied, a switch keeps the person values, exit restores them', async function () {
    this.timeout(60_000);
    await config().update('window.title', 'My window', vscode.ConfigurationTarget.Global);
    await config().update('breadcrumbs.enabled', true, vscode.ConfigurationTarget.Global);
    subs.push(
      api.registerFocusProfile({
        id: 'acme.calm',
        settings: { 'window.title': 'Acme', 'breadcrumbs.enabled': false },
      }),
      api.registerFocusProfile({
        id: 'other.quiet',
        settings: { 'window.title': 'Other', 'editor.minimap.enabled': false },
      }),
    );
    await config().update('escurel.focus.profile', 'acme.calm', vscode.ConfigurationTarget.Global);
    await vscode.commands.executeCommand('escurel.focusMode.enter', {
      silent: true,
      overview: false,
    });
    assert.equal(user('window.title'), 'Acme');
    assert.equal(user('breadcrumbs.enabled'), false);
    assert.equal(
      user('workbench.statusBar.visible'),
      undefined,
      'the built-in list is not applied',
    );

    await config().update(
      'escurel.focus.profile',
      'other.quiet',
      vscode.ConfigurationTarget.Global,
    );
    // The switch writes several settings one after the other; wait for the last of them.
    await until(
      () =>
        user('window.title') === 'Other' &&
        user('editor.minimap.enabled') === false &&
        user('breadcrumbs.enabled') === true,
    );
    assert.equal(
      user('breadcrumbs.enabled'),
      true,
      'given back the moment the other profile took over',
    );
    assert.equal(user('editor.minimap.enabled'), false);

    await vscode.commands.executeCommand('escurel.focusMode.exit');
    assert.equal(user('window.title'), 'My window');
    assert.equal(user('breadcrumbs.enabled'), true);
    assert.equal(user('editor.minimap.enabled'), undefined);
  });

  test('a profile that names a setting of Escurel is refused', () => {
    assert.throws(() =>
      api.registerFocusProfile({
        id: 'evil.one',
        settings: { 'escurel.gatewayUrl': 'http://x.example' },
      }),
    );
  });
});

async function until(f: () => boolean, ms = 10_000): Promise<void> {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error('timed out waiting for the setting');
    await new Promise((r) => setTimeout(r, 100));
  }
}
