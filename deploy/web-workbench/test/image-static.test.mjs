// Static checks of the image recipe (no docker): the overlay hooks are wired, and nothing operator-local ships.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const wb = join(dirname(fileURLToPath(import.meta.url)), '..');
const docker = readFileSync(join(wb, 'Dockerfile'), 'utf8');

test('the image ships the overlay loader and installer and puts escurel-overlay-install on the PATH', () => {
  assert.match(docker, /overlay\.mjs deploy\/web-workbench\/overlay-install\.mjs/);
  assert.match(docker, /\/usr\/local\/bin\/escurel-overlay-install/);
  assert.match(docker, /&& escurel-overlay-install/, 'the base build runs the installer once, as the code-server layout assertion');
  assert.match(docker, /test ! -e \/opt\/escurel\/overlay/, 'the base image carries no overlay of its own');
});

test('login/used-colors.mjs travels with the login directory', () => {
  assert.match(docker, /COPY deploy\/web-workbench\/login \/opt\/escurel\/login/);
});

test('docs and tests stay out of the image context', () => {
  const ignore = readFileSync(join(wb, 'Dockerfile.dockerignore'), 'utf8');
  for (const p of ['deploy/web-workbench/examples', 'deploy/web-workbench/test', 'deploy/web-workbench/probe']) assert.ok(ignore.includes(p), p);
});

test('the entrypoint validates the overlay before it renders settings or starts code-server', () => {
  const e = readFileSync(join(wb, 'entrypoint.sh'), 'utf8');
  assert.ok(e.indexOf('overlay.mjs" check') > 0 && e.indexOf('overlay.mjs" check') < e.indexOf('render-settings.mjs'));
  assert.ok(e.indexOf('overlay.mjs" check') < e.indexOf('exec dumb-init'));
});
