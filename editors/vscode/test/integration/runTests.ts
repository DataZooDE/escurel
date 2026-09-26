// The integration suites, each against a gateway shaped for it.
//
//   suite/    the corpus suites (M1, M2): the crm-demo seed, its pre-seeded
//             inbox events intact, and NO runner — nothing must consume those
//             events, because tests assert they are in the Inbox.
//   cascade/  the cascade suites (M3): a seed of its own with an EMPTY inbox,
//             and a real `escurel-runner` driving the real echo harness.
//
// The two cannot share a gateway. The echo harness folds the oldest inbox event
// carrying a target instance, not the trigger its run was dispatched for, so with
// crm-demo's seeded events present a run reaches for one of those and drafts
// against a page the test knows nothing about. Two gateways, two hosts, one
// command.
//
// Needs built binaries:
//   ESCUREL_SERVER_BIN=… ESCUREL_RUNNER_BIN=… npm run test:integration
// (defaults: ../../target/release/{escurel-server,escurel-runner}). Without the
// runner binary the cascade run is skipped, not failed.
//
//   ESCUREL_TEST_GREP=<regex>   narrow both runs to matching tests
import { runTests } from '@vscode/test-electron';
import { spawn, type ChildProcess } from 'node:child_process';
import { cpSync, existsSync, mkdtempSync, openSync, writeFileSync, mkdirSync } from 'node:fs';
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

interface Gateway {
  url: string;
  data: string;
  stop: () => void;
}

function startGateway(bin: string, port: number, seed: string): Gateway {
  const data = mkdtempSync(join(tmpdir(), 'escurel-vsx-'));
  const gw: ChildProcess = spawn(bin, [], {
    env: {
      ...process.env,
      ESCUREL_SERVER_DATA_DIR: data,
      ESCUREL_SERVER_LISTEN_HTTP: `127.0.0.1:${port}`,
      ESCUREL_OBSERVABILITY_METRICS_LISTEN: '127.0.0.1:0',
      ESCUREL_TENANT: 'vsx',
      ESCUREL_EMBEDDING_PROVIDER: 'zero',
      ESCUREL_SEED_DIR: seed,
    },
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  return { url: `http://127.0.0.1:${port}`, data, stop: () => gw.kill('SIGTERM') };
}

/** A workspace whose settings point the extension at `gatewayUrl`. */
function workspaceFor(gatewayUrl: string): string {
  const ws = mkdtempSync(join(tmpdir(), 'escurel-vsx-ws-'));
  mkdirSync(join(ws, '.vscode'));
  writeFileSync(
    join(ws, '.vscode', 'settings.json'),
    JSON.stringify({ 'escurel.gatewayUrl': gatewayUrl }),
  );
  return ws;
}

/**
 * The crm-demo corpus with `test/integration/seed/skills` laid over it, so the
 * corpus suites keep every page and event they assert on while the cascade
 * skills exist for anything that wants to read them.
 */
function corpusSeed(repo: string, root: string): string {
  const dir = mkdtempSync(join(tmpdir(), 'escurel-vsx-seed-'));
  cpSync(join(repo, 'examples', 'crm-demo'), dir, { recursive: true });
  cpSync(join(root, 'test', 'integration', 'seed'), dir, { recursive: true });
  return dir;
}

async function main(): Promise<void> {
  const root = resolve(__dirname, '..', '..', '..');
  const repo = resolve(root, '..', '..');
  const bin = process.env.ESCUREL_SERVER_BIN ?? join(repo, 'target', 'release', 'escurel-server');
  const runnerBin =
    process.env.ESCUREL_RUNNER_BIN ?? join(repo, 'target', 'release', 'escurel-runner');
  const grep = process.env.ESCUREL_TEST_GREP ?? '';
  const basePort = 18000 + Math.floor(Math.random() * 900);

  // ── the corpus run ───────────────────────────────────────────────
  const corpus = startGateway(bin, basePort, corpusSeed(repo, root));
  try {
    await waitForHealth(corpus.url, 60_000);
    await runTests({
      extensionDevelopmentPath: root,
      extensionTestsPath: resolve(__dirname, 'suite', 'index.js'),
      launchArgs: [workspaceFor(corpus.url), '--disable-extensions', '--disable-workspace-trust'],
      extensionTestsEnv: { ESCUREL_TEST_GATEWAY: corpus.url, ESCUREL_TEST_GREP: grep },
    });
  } finally {
    corpus.stop();
  }

  // ── the cascade run ──────────────────────────────────────────────
  const cascade = startGateway(bin, basePort + 2, join(root, 'test', 'integration', 'seed'));
  let runner: ChildProcess | undefined;
  try {
    await waitForHealth(cascade.url, 60_000);

    // The runner polls the inbox only with a tenant AND a token. This gateway has
    // no verifier, so it ignores the bearer entirely: the placeholder is what
    // turns the poller on, not a credential.
    if (existsSync(runnerBin)) {
      const log = join(cascade.data, 'runner.log');
      runner = spawn(runnerBin, [], {
        env: {
          ...process.env,
          ESCUREL_RUNNER_GATEWAY_URL: cascade.url,
          ESCUREL_RUNNER_TENANT: 'vsx',
          ESCUREL_RUNNER_TOKEN: 'no-verifier',
          ESCUREL_RUNNER_HARNESS: 'echo',
          ESCUREL_RUNNER_LISTEN: `127.0.0.1:${basePort + 3}`,
          ESCUREL_RUNNER_LEDGER_PATH: join(cascade.data, 'ledger.duckdb'),
          ESCUREL_RUNNER_POLL_INTERVAL: '250ms',
        },
        // Its log goes to a file: chatty at a 250 ms poll, and when a cascade does
        // not happen this file is the only thing that says why.
        stdio: ['ignore', openSync(log, 'a'), openSync(log, 'a')],
      });
      console.log(`runner log: ${log}`);
    } else {
      console.warn(`no runner at ${runnerBin}: the cascade suites will skip themselves`);
    }

    await runTests({
      extensionDevelopmentPath: root,
      extensionTestsPath: resolve(__dirname, 'cascade', 'index.js'),
      launchArgs: [workspaceFor(cascade.url), '--disable-extensions', '--disable-workspace-trust'],
      extensionTestsEnv: {
        ESCUREL_TEST_GATEWAY: cascade.url,
        ESCUREL_TEST_GREP: grep,
        ESCUREL_TEST_RUNNER: runner ? '1' : '',
      },
    });
  } finally {
    runner?.kill('SIGTERM');
    cascade.stop();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
