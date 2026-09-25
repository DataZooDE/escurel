// The integration suite: a REAL `escurel-server` (no verifier, the crm-demo
// seed) plus a real VS Code Extension Development Host running
// test/integration/suite against it. Needs a built gateway binary:
//   ESCUREL_SERVER_BIN=/path/to/escurel-server npm run test:integration
// (default: ../../target/release/escurel-server).
import { runTests } from '@vscode/test-electron';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

async function waitForHealth(url: string, ms: number): Promise<void> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      if ((await fetch(`${url}/healthz`)).ok) return;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`escurel-server did not answer /healthz within ${ms} ms`);
}

async function main(): Promise<void> {
  const root = resolve(__dirname, '..', '..', '..');
  const repo = resolve(root, '..', '..');
  const bin = process.env.ESCUREL_SERVER_BIN ?? join(repo, 'target', 'release', 'escurel-server');
  const port = 18000 + Math.floor(Math.random() * 1000);
  const gatewayUrl = `http://127.0.0.1:${port}`;
  const data = mkdtempSync(join(tmpdir(), 'escurel-vsx-'));
  const gw: ChildProcess = spawn(bin, [], {
    env: {
      ...process.env,
      ESCUREL_SERVER_DATA_DIR: data,
      ESCUREL_SERVER_LISTEN_HTTP: `127.0.0.1:${port}`,
      ESCUREL_OBSERVABILITY_METRICS_LISTEN: '127.0.0.1:0',
      ESCUREL_TENANT: 'vsx',
      ESCUREL_EMBEDDING_PROVIDER: 'zero',
      ESCUREL_SEED_DIR: join(repo, 'examples', 'crm-demo'),
    },
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  try {
    await waitForHealth(gatewayUrl, 60_000);
    // The workspace the host opens carries the gateway setting.
    const ws = mkdtempSync(join(tmpdir(), 'escurel-vsx-ws-'));
    mkdirSync(join(ws, '.vscode'));
    writeFileSync(
      join(ws, '.vscode', 'settings.json'),
      JSON.stringify({ 'escurel.gatewayUrl': gatewayUrl }),
    );
    await runTests({
      extensionDevelopmentPath: root,
      extensionTestsPath: resolve(__dirname, 'suite', 'index.js'),
      launchArgs: [ws, '--disable-extensions', '--disable-workspace-trust'],
      extensionTestsEnv: { ESCUREL_TEST_GATEWAY: gatewayUrl },
    });
  } finally {
    gw.kill('SIGTERM');
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
