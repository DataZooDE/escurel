// The web workbench's bootstrap enters focus mode once per browser profile, not on every load: a person who
// left focus mode keeps the classic layout. Run with `node --test deploy/web-workbench/test/bootstrap.test.mjs`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import Module from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

function load(calls, config) {
  const fake = {
    commands: { executeCommand: async (id, ...args) => void calls.push(['cmd', id, ...args]) },
    workspace: { getConfiguration: () => ({ update: async (k, v) => void calls.push(['cfg', k, v]) }) },
    ConfigurationTarget: { Global: 1 },
  };
  const orig = Module._load;
  Module._load = (req, ...rest) => (req === 'vscode' ? fake : orig(req, ...rest));
  const file = path.join(here, '..', 'bootstrap', 'extension.js');
  delete require.cache[require.resolve(file)];
  try {
    return require(file);
  } finally {
    Module._load = orig;
  }
}

const memento = () => {
  const m = new Map();
  return { get: (k) => m.get(k), update: async (k, v) => void m.set(k, v) };
};

test('the first load enters focus mode and hides the activity bar', async () => {
  const calls = [];
  const globalState = memento();
  await load(calls).activate({ globalState });
  assert.ok(calls.some((c) => c[1] === 'escurel.focusMode.enter'));
  assert.ok(calls.some((c) => c[0] === 'cfg' && c[1] === 'workbench.activityBar.location' && c[2] === 'hidden'));
});

test('a later load only reveals the views: it does not re-force the layout', async () => {
  const globalState = memento();
  await load([]).activate({ globalState });
  const calls = [];
  await load(calls).activate({ globalState });
  assert.ok(calls.some((c) => c[1] === 'workbench.view.extension.escurel'));
  assert.ok(!calls.some((c) => c[1] === 'escurel.focusMode.enter'), 'focus mode forced again');
  assert.ok(!calls.some((c) => c[0] === 'cfg'), 'activity bar forced again');
});
