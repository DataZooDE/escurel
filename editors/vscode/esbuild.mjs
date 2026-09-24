// Two bundles: the extension host (CommonJS, `vscode` external) and one ESM
// bundle per webview under webview/<name>/main.ts → dist/webview/<name>.js.
import * as esbuild from 'esbuild';
import { readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');

const webviews = readdirSync('webview', { withFileTypes: true })
  .filter((d) => d.isDirectory() && existsSync(join('webview', d.name, 'main.ts')))
  .map((d) => d.name);

/** @type {import('esbuild').BuildOptions} */
const host = {
  entryPoints: ['src/extension.ts'],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node22',
  external: ['vscode'],
  outfile: 'dist/extension.js',
  sourcemap: !production,
  minify: production,
  logLevel: 'info',
};

/** @type {import('esbuild').BuildOptions} */
const webview = {
  entryPoints: Object.fromEntries(webviews.map((n) => [n, `webview/${n}/main.ts`])),
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: 'es2022',
  outdir: 'dist/webview',
  tsconfig: 'tsconfig.webview.json',
  sourcemap: !production,
  minify: production,
  logLevel: 'info',
};

/** @type {import('esbuild').BuildOptions} */
const integration = {
  entryPoints: ['test/integration/runTests.ts', 'test/integration/suite/index.ts'],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node22',
  external: ['vscode', 'mocha', '@vscode/test-electron'],
  outdir: 'dist/test/integration',
  logLevel: 'info',
};

/** @type {import('esbuild').BuildOptions} */
const harness = {
  entryPoints: { harness: 'test/visual/harness/main.ts' },
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: 'es2022',
  outdir: 'dist/test/visual',
  tsconfig: 'tsconfig.webview.json',
  logLevel: 'info',
};

const configs = [
  host,
  ...(webviews.length ? [webview] : []),
  harness,
  ...(existsSync('test/integration/runTests.ts') ? [integration] : []),
];

if (watch) {
  const contexts = await Promise.all(configs.map((c) => esbuild.context(c)));
  await Promise.all(contexts.map((c) => c.watch()));
  console.log('watching…');
} else {
  await Promise.all(configs.map((c) => esbuild.build(c)));
}
