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
import type { GatewayInfo } from './gatewayInfo';
import { runnerEnv } from './runnerEnv';
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

/** Start the verifying gateway and read its connection line; it stays up until SIGTERM. */
async function startVerifyingGateway(
  bin: string,
  seed: string,
): Promise<{ child: ChildProcess; info: GatewayInfo }> {
  const child = spawn(bin, ['--tenant', 'vsx', '--seed', seed, '--subject', 'alice'], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const line = await new Promise<string>((resolveLine, reject) => {
    let buffered = '';
    const timer = setTimeout(
      () => reject(new Error('escurel-test-gateway printed nothing in 60 s')),
      60_000,
    );
    child.stdout!.on('data', (chunk: Buffer) => {
      buffered += chunk.toString();
      const nl = buffered.indexOf('\n');
      if (nl >= 0) {
        clearTimeout(timer);
        resolveLine(buffered.slice(0, nl));
      }
    });
    child.on('exit', (code) =>
      reject(new Error(`escurel-test-gateway exited with ${code} before it was ready`)),
    );
  });
  return { child, info: JSON.parse(line) as GatewayInfo };
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

/**
 * The VS Code the suites run in. Pinned, because `runTests` otherwise downloads whatever
 * "stable" is today: 1.140.0 crashed on start (SIGTRAP) in the environment the suites had
 * passed in the day before, with no change of ours. Bump it deliberately, with the suites
 * run, not by waiting for a release to break them. `ESCUREL_VSCODE_VERSION=insiders` or any
 * version string overrides it, which is how to try a new release before adopting it.
 */
const VSCODE_VERSION = process.env.ESCUREL_VSCODE_VERSION ?? '1.139.1';

async function main(): Promise<void> {
  const root = resolve(__dirname, '..', '..', '..');
  const repo = resolve(root, '..', '..');
  const bin = process.env.ESCUREL_SERVER_BIN ?? join(repo, 'target', 'release', 'escurel-server');
  const runnerBin =
    process.env.ESCUREL_RUNNER_BIN ?? join(repo, 'target', 'release', 'escurel-runner');
  const gatewayBin =
    process.env.ESCUREL_TEST_GATEWAY_BIN ?? join(repo, 'target', 'release', 'escurel-test-gateway');
  const grep = process.env.ESCUREL_TEST_GREP ?? '';
  const basePort = 18000 + Math.floor(Math.random() * 900);

  // ── the corpus run ───────────────────────────────────────────────
  const corpus = startGateway(bin, basePort, corpusSeed(repo, root));
  try {
    await waitForHealth(corpus.url, 60_000);
    await runTests({
      version: VSCODE_VERSION,
      extensionDevelopmentPath: root,
      extensionTestsPath: resolve(__dirname, 'suite', 'index.js'),
      launchArgs: [workspaceFor(corpus.url), '--disable-extensions', '--disable-workspace-trust'],
      extensionTestsEnv: { ESCUREL_TEST_GATEWAY: corpus.url, ESCUREL_TEST_GREP: grep },
    });
  } finally {
    corpus.stop();
  }

  // ── the cascade run ──────────────────────────────────────────────
  //
  // Against a gateway that VERIFIES tokens, started by `escurel-test-gateway`: the same
  // in-process gateway and issuer the Rust suites use, so the claims cannot drift. It has to
  // verify: only a verified token can prove which run wrote something, so a runner minting
  // per-run tokens against a gateway with no verifier has its tokens ignored, the agent's draft
  // carries no run, the lineage never shows the changeset, and promoting never cascades.
  let runner: ChildProcess | undefined;
  let gateway: ChildProcess | undefined;
  try {
    if (!existsSync(gatewayBin) || !existsSync(runnerBin)) {
      console.warn(
        `no ${existsSync(gatewayBin) ? '' : `${gatewayBin} `}${existsSync(runnerBin) ? '' : runnerBin}: the cascade suites will skip themselves`,
      );
    }
    let info: GatewayInfo | undefined;
    if (existsSync(gatewayBin) && existsSync(runnerBin)) {
      const started = await startVerifyingGateway(
        gatewayBin,
        join(root, 'test', 'integration', 'seed'),
      );
      gateway = started.child;
      info = started.info;
      const dir = mkdtempSync(join(tmpdir(), 'escurel-vsx-runner-'));
      const log = join(dir, 'runner.log');
      // MINTED mode: no `ESCUREL_RUNNER_TOKEN`; given an issuer, a key id and the signing key
      // the runner signs a token per run, scoped to the agent and carrying the run's identity.
      runner = spawn(runnerBin, [], {
        env: runnerEnv(process.env, info, { port: basePort + 3, dir }),
        // A file, not the console: chatty at a 250 ms poll, and when a cascade does not happen
        // this is the only thing that says why.
        stdio: ['ignore', openSync(log, 'a'), openSync(log, 'a')],
      });
      console.log(`runner log: ${log}`);
    }

    await runTests({
      version: VSCODE_VERSION,
      extensionDevelopmentPath: root,
      extensionTestsPath: resolve(__dirname, 'cascade', 'index.js'),
      launchArgs: [
        workspaceFor(info?.gateway_url ?? 'http://127.0.0.1:1'),
        '--disable-extensions',
        '--disable-workspace-trust',
      ],
      extensionTestsEnv: {
        ESCUREL_TEST_GATEWAY: info?.gateway_url ?? '',
        ESCUREL_TEST_GREP: grep,
        ESCUREL_TEST_RUNNER: runner ? '1' : '',
        // The suite hands this to the extension through its API (`useStaticToken`); nothing in
        // the shipped extension reads it, so a running install cannot be given a credential.
        ESCUREL_TEST_BEARER: info?.bearer ?? '',
        ESCUREL_TEST_ADMIN_BEARER: info?.admin_bearer ?? '',
        ESCUREL_TEST_SUBJECT: 'alice',
      },
    });
  } finally {
    runner?.kill('SIGTERM');
    gateway?.kill('SIGTERM');
  }
}

/**
 * Electron needs a display, and a session with none — an SSH login, a locked desktop, CI —
 * kills VS Code on start with SIGTRAP and no message. The same suite crashed in one session
 * and passed in the next for that reason alone, so the harness supplies its own: with no
 * display it re-runs itself under `xvfb-run`, which makes the suites independent of
 * whoever is logged in.
 */
function needsVirtualDisplay(): boolean {
  return !process.env.DISPLAY && !process.env.WAYLAND_DISPLAY && !process.env.ESCUREL_IN_XVFB;
}

if (needsVirtualDisplay()) {
  const child = spawn('xvfb-run', ['-a', process.execPath, ...process.argv.slice(1)], {
    stdio: 'inherit',
    env: { ...process.env, ESCUREL_IN_XVFB: '1' },
  });
  child.on('error', (e) => {
    console.error(`no display and xvfb-run could not start (${e.message}); install xvfb`);
    process.exit(1);
  });
  child.on('exit', (code) => process.exit(code ?? 1));
} else {
  main().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
