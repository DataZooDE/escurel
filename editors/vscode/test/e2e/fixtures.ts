import {
  test as base,
  expect,
  chromium,
  type Browser,
  type FrameLocator,
  type Page,
} from '@playwright/test';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, readFileSync, mkdirSync, openSync, closeSync } from 'node:fs';
import { createServer } from 'node:net';
import { createServer as createHttpServer, type Server as HttpServer } from 'node:http';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

/**
 * A live stack for end-to-end tests, per worker:
 *
 *   Xvfb  ->  demo/run.sh start  ->  VS Code (the real extension, a signed-in window)
 *               |-- escurel-test-gateway (verifies tokens)
 *               `-- escurel-runner (minted mode, echo harness, which idles ~6 s so a run is
 *                   still live long enough to be clicked)
 *
 * and Playwright attached to the window over CDP. It runs on its OWN virtual display and profile, so
 * it never touches a window you have open. Chromium's site isolation is turned off for it: a VS Code
 * webview is an out-of-process iframe that Playwright cannot see into otherwise.
 *
 * Needs the built extension (`npm run build`) and the release binaries (`ESCUREL_BIN_DIR`, default
 * <repo>/target/release: escurel-test-gateway, escurel-runner).
 */
const EXT = resolve(__dirname, '..', '..');
const REPO = resolve(EXT, '..', '..');

async function freePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => resolvePort(port));
    });
    s.on('error', reject);
  });
}

/** A tool's structured result: whatever the gateway returned, read by the test that asked. */
export type ToolResult = Record<string, unknown>;

export interface Stack {
  page: Page;
  workspaceDir: string;
  display: string;
  /** What the demo driver left behind: the root events and changesets of the story. */
  story: { rootA: string; rootB: string; promoted: string; awaiting: string };
  /** The S2D stories' root events (only with the `s2d` option). */
  s2dStory?: { exception: string; transport: string; ltb: string };
  gatewayUrl: string;
  /** The demo's home directory: pid files and the ports of the demo's outside systems. */
  home: string;
  /** Call a gateway tool as the signed-in human (alice), for setting up or checking state. */
  call: (name: string, args: Record<string, unknown>, admin?: boolean) => Promise<ToolResult>;
  /** Console and page errors collected since the window opened. */
  errors: string[];
  shot: (name: string) => Promise<void>;
  setGeminiPlanTarget: (pageId: string, revision: string) => void;
  geminiRequests: Record<string, unknown>[];
  /** Authenticated Anofox Evolve tool call when the cross-repository service fixture is enabled. */
  evolveCall: (name: string, args: Record<string, unknown>) => Promise<ToolResult>;
}

// See playwright.config.ts: ESCUREL_E2E_SLOW stretches every wait on an overloaded machine.
const SLOW = Number(process.env.ESCUREL_E2E_SLOW) || 1;

export const test = base.extend<
  object,
  {
    stack: Stack;
    runnerHarness: 'echo' | 'gemini';
    evolveAgentBin: string | undefined;
    /** Load the Source-to-Deliver demo (hetzner seed) too; the other scenarios count rows and must not see it. */
    s2d: boolean;
    /** Names the file's window: files that set different values never share one (state of an earlier file leaked into a later one). */
    suite: string;
    /** Whether the window opens in the calm focus view (the demo's default). Off: the classic IDE look the view tests drive. */
    focus: boolean;
  }
>({
  suite: ['', { scope: 'worker', option: true }],
  focus: [false, { option: true, scope: 'worker' }],
  s2d: [false, { scope: 'worker', option: true }],
  runnerHarness: ['echo', { scope: 'worker', option: true }],
  evolveAgentBin: [undefined, { scope: 'worker', option: true }],
  stack: [
    async ({ runnerHarness, evolveAgentBin, s2d, focus }, use) => {
      const home = mkdtempSync(join(homedir(), '.cache', 'escurel-e2e-'));
      let geminiPlanTarget: { pageId: string; revision: string } | undefined;
      const geminiRequests: Record<string, unknown>[] = [];
      let modelServer: HttpServer | undefined;
      let modelBase: string | undefined;
      if (runnerHarness === 'gemini') {
        modelServer = createHttpServer(async (request, response) => {
          let body = '';
          for await (const chunk of request) body += String(chunk);
          const prompt = JSON.parse(body) as Record<string, unknown>;
          const contents = prompt.contents as unknown[] | undefined;
          const isEvolve = !!geminiPlanTarget && JSON.stringify(prompt).includes('evolve_run');
          let parts: unknown[] = [{ text: 'No page changes requested.' }];
          if (isEvolve) {
            geminiRequests.push(prompt);
            if (contents?.length === 1) {
              const firstRequest = JSON.stringify(prompt);
              if (
                !firstRequest.includes(geminiPlanTarget.pageId) ||
                !firstRequest.includes(geminiPlanTarget.revision)
              ) {
                response.writeHead(422).end('Evolve plan prompt omitted the frozen problem page');
                return;
              }
              parts = [
                {
                  functionCall: {
                    name: 'expand',
                    args: {
                      page_id: geminiPlanTarget.pageId,
                      raw: true,
                    },
                  },
                },
              ];
            } else if (contents?.length === 3) {
              parts = [
                {
                  functionCall: {
                    name: 'report_progress',
                    args: {
                      plan: [
                        {
                          step: 'Review the frozen source, holdout and V2 budget',
                          status: 'pending',
                        },
                        {
                          step: 'Run bounded DuckDB search after owner approval',
                          status: 'pending',
                        },
                      ],
                    },
                  },
                },
              ];
            } else {
              parts = [{ text: 'Plan reported for the owner to review.' }];
            }
          }
          response.setHeader('content-type', 'application/json');
          response.end(JSON.stringify({ candidates: [{ content: { parts } }] }));
        });
        await new Promise<void>((resolveListen) =>
          modelServer!.listen(0, '127.0.0.1', resolveListen),
        );
        const address = modelServer.address();
        if (!address || typeof address === 'string') throw new Error('Gemini stub has no port');
        modelBase = `http://127.0.0.1:${address.port}`;
      }
      const artifacts = resolve(__dirname, 'artifacts');
      mkdirSync(artifacts, { recursive: true });
      // Xvfb picks a FREE display itself and writes its number to fd 3 once it is ready to accept
      // connections: no random display number that can collide with a parallel run, and no sleep.
      const xvfb: ChildProcess = spawn(
        'Xvfb',
        ['-displayfd', '3', '-screen', '0', '1700x1000x24'],
        { stdio: ['ignore', 'ignore', 'ignore', 'pipe'] },
      );
      const display = await new Promise<string>((resolveDisplay, reject) => {
        let buf = '';
        const timer = setTimeout(() => reject(new Error('Xvfb did not report a display')), 20_000);
        xvfb.stdio[3]!.on('data', (d: Buffer) => {
          buf += d.toString();
          if (buf.includes('\n')) {
            clearTimeout(timer);
            resolveDisplay(`:${buf.trim()}`);
          }
        });
        xvfb.on('exit', (code) => reject(new Error(`Xvfb exited (${code})`)));
      });
      const cdpPort = await freePort();
      const evolvePort = evolveAgentBin ? await freePort() : undefined;
      const configuredEvolveUrl = evolvePort ? `http://127.0.0.1:${evolvePort}` : undefined;
      const bin = process.env.ESCUREL_BIN_DIR ?? join(REPO, 'target', 'release');
      const env = {
        ...process.env,
        DISPLAY: display,
        WAYLAND_DISPLAY: '',
        XDG_SESSION_TYPE: 'x11',
        ESCUREL_DEMO_HOME: home,
        ESCUREL_DEMO_CDP_PORT: String(cdpPort),
        ESCUREL_DEMO_CODE_ARGS:
          '--ozone-platform=x11 --disable-site-isolation-trials --disable-features=IsolateOrigins,site-per-process' +
          (process.env.CI ? ' --no-sandbox' : '') +
          // A machine whose GPU is wedged hangs Electron before it paints: ESCUREL_E2E_EXTRA_CODE_ARGS=--disable-gpu.
          (process.env.ESCUREL_E2E_EXTRA_CODE_ARGS
            ? ` ${process.env.ESCUREL_E2E_EXTRA_CODE_ARGS}`
            : ''),
        ESCUREL_TEST_GATEWAY_BIN: join(bin, 'escurel-test-gateway'),
        ESCUREL_RUNNER_BIN: join(bin, 'escurel-runner'),
        ESCUREL_CLI_BIN: join(bin, 'escurel'),
        ESCUREL_DEMO_S2D: s2d ? '1' : '0',
        ESCUREL_ECHO_SLEEP_MS: '6000',
        // No zoom: Playwright maps clicks into a nested webview with the page's own scale, and a zoomed
        // window (the demo's default) puts them on the wrong element.
        ESCUREL_DEMO_ZOOM: '0',
        // Keep VS Code's modal confirmation inside the CDP window so the
        // approval text and deliberate human click are observable end to end.
        ESCUREL_DEMO_DIALOG_STYLE: 'custom',
        ESCUREL_DEMO_EVOLVE_SEED: '1',
        ESCUREL_DEMO_RUNNER_HARNESS: runnerHarness,
        ...(configuredEvolveUrl ? { ESCUREL_DEMO_EVOLVE_ENDPOINT: configuredEvolveUrl } : {}),
        ...(modelBase
          ? {
              ESCUREL_GEMINI_API_KEY: 'deterministic-native-plan-key',
              ESCUREL_RUNNER_GEMINI_BASE_URL: modelBase,
            }
          : {}),
        ESCUREL_DEMO_FOCUS: focus ? '1' : '0',
      };
      const run = join(EXT, 'demo', 'run.sh');
      let browser: Browser | undefined;
      let evolveProcess: ChildProcess | undefined;
      let evolveLogFd: number | undefined;
      try {
        execFileSync(run, ['start'], { env, stdio: 'inherit', timeout: 240_000 * SLOW });

        const info = JSON.parse(readFileSync(join(home, 'gateway.json'), 'utf8').split('\n')[0]!);
        const story = JSON.parse(readFileSync(join(home, 'story.json'), 'utf8'));
        const s2dStory = s2d
          ? (JSON.parse(readFileSync(join(home, 's2d-story.json'), 'utf8')) as Stack['s2dStory'])
          : undefined;
        const bearer = () =>
          JSON.parse(readFileSync(join(home, 'bearer.json'), 'utf8')) as {
            bearer: string;
            admin_bearer: string;
          };
        let evolveUrl: string | undefined;
        if (evolveAgentBin) {
          // A dynamic import: Playwright loads this file as CommonJS and the shared module is an ES module.
          const { evolveAgentEnv } = await import('../../demo/evolve-env.mjs');
          const port = evolvePort!;
          evolveUrl = configuredEvolveUrl;
          evolveLogFd = openSync(join(home, 'evolve.log'), 'w');
          evolveProcess = spawn(
            evolveAgentBin,
            ['serve', '--addr', `127.0.0.1:${port}`, '--db', join(home, 'evolve.duckdb')],
            {
              env: {
                ...process.env,
                ...evolveAgentEnv({
                  gatewayUrl: info.gateway_url,
                  adminBearer: bearer().admin_bearer,
                  issuerUrl: info.issuer_url,
                  tenant: 'vsx',
                }),
                GEMINI_API_KEY: 'unused-seed-only-test-key',
              },
              stdio: ['ignore', evolveLogFd, evolveLogFd],
            },
          );
          let up = false;
          for (let i = 0; i < 100 && !up; i += 1) {
            if (evolveProcess.exitCode !== null) break;
            try {
              up = (await fetch(`${evolveUrl}/healthz`)).ok;
            } catch {
              /* starting */
            }
            if (!up) await new Promise((r) => setTimeout(r, 100));
          }
          if (!up)
            throw new Error(
              `Evolve service did not start: ${readFileSync(join(home, 'evolve.log'), 'utf8')}`,
            );
        }

        for (let i = 0; i < 60 && !browser; i += 1) {
          try {
            browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
          } catch {
            await new Promise((r) => setTimeout(r, 500));
          }
        }
        if (!browser) {
          const codeLog = readFileSync(join(home, 'code.log'), 'utf8');
          throw new Error(
            `could not attach to the VS Code window (display=${display}, xvfbExit=${xvfb.exitCode}, ` +
              `codeLog=${codeLog.slice(-6000)})`,
          );
        }
        // The window may not have a page yet when the debugger first answers.
        let page: Page | undefined;
        for (let i = 0; i < 120 && !page; i += 1) {
          page = browser
            .contexts()[0]
            ?.pages()
            .find((p) => /workbench/.test(p.url()));
          if (!page) await new Promise((r) => setTimeout(r, 500));
        }
        if (!page) throw new Error('the VS Code window never showed a workbench page');
        await page.waitForSelector('.monaco-workbench', { timeout: 60_000 * SLOW });
        const errors: string[] = [];
        page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
        page.on('console', (m) => {
          if (m.type() === 'error') errors.push(`console: ${m.text()}`);
        });

        const stack: Stack = {
          page,
          workspaceDir: join(home, 'workspace'),
          display,
          story,
          s2dStory,
          gatewayUrl: info.gateway_url,
          home,
          errors,
          call: async (name, args, admin = false) => {
            const tok = bearer();
            const res = await fetch(`${info.gateway_url}/mcp`, {
              method: 'POST',
              headers: {
                'content-type': 'application/json',
                authorization: `Bearer ${admin ? tok.admin_bearer : tok.bearer}`,
              },
              body: JSON.stringify({
                jsonrpc: '2.0',
                id: 1,
                method: 'tools/call',
                params: { name, arguments: args },
              }),
            });
            const body = (await res.json()) as {
              error?: unknown;
              result: { structuredContent: ToolResult };
            };
            if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
            return body.result.structuredContent;
          },
          shot: async (name) => {
            await page.screenshot({ path: join(artifacts, `${name}.png`) });
          },
          setGeminiPlanTarget: (pageId, revision) => {
            geminiPlanTarget = { pageId, revision };
          },
          geminiRequests,
          evolveCall: async (name, args) => {
            if (!evolveUrl) throw new Error('Evolve service fixture is not enabled');
            const res = await fetch(evolveUrl, {
              method: 'POST',
              headers: {
                'content-type': 'application/json',
                authorization: `Bearer ${bearer().bearer}`,
                'X-Triton-Tool': name,
              },
              body: JSON.stringify(args),
            });
            const body = (await res.json()) as ToolResult;
            if (!res.ok) throw new Error(`${name}: HTTP ${res.status}: ${JSON.stringify(body)}`);
            return body;
          },
        };
        await use(stack);
      } finally {
        await browser?.close().catch(() => undefined);
        if (evolveProcess && evolveProcess.exitCode === null && evolveProcess.signalCode === null) {
          const child = evolveProcess;
          child.kill('SIGTERM');
          await Promise.race([
            new Promise<void>((resolveExit) => child.once('exit', () => resolveExit())),
            new Promise<void>((resolveTimeout) => setTimeout(resolveTimeout, 3_000)),
          ]);
          if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
        }
        if (evolveLogFd !== undefined) closeSync(evolveLogFd);
        try {
          execFileSync(run, ['stop'], { env, stdio: 'ignore' });
        } catch {
          /* already gone */
        }
        xvfb.kill();
        if (modelServer)
          await new Promise<void>((resolveClose) => modelServer!.close(() => resolveClose()));
      }
    },
    { scope: 'worker', timeout: 300_000 * SLOW },
  ],
});

export { expect };

/**
 * The VISIBLE webview (an iframe inside an iframe) that contains `selector`.
 *
 * VS Code keeps the webview of every open tab in the DOM and hides the inactive ones with
 * `visibility: hidden`, so a plain `iframe.webview` also matches the tab you are not looking at,
 * and a click on it lands on whatever is on top.
 */
export async function webviewWith(
  page: Page,
  selector: string,
  /** Text the wanted page shows (its page id): tells two page webviews apart. */
  contains?: string,
): Promise<FrameLocator> {
  // A tooltip left by hovering a tree row floats over the editor and intercepts clicks on the page
  // (seen over the Change-rating button): leave the sidebar, and let it go, before touching a webview.
  await page.mouse.move(760, 520);
  await expect(page.locator('.context-view .monaco-hover')).toHaveCount(0);
  const outer = page.locator('iframe.webview:visible');
  let found: FrameLocator | undefined;
  await expect
    .poll(
      async () => {
        // While VS Code switches editor tabs the outgoing webview is still visible for a moment, and
        // an index-based locator picked it (and shifted as webviews came and went). Take the most
        // recent matching webview and pin it by its own name, which does not move.
        const n = await outer.count();
        for (let i = n - 1; i >= 0; i -= 1) {
          const name = await outer.nth(i).getAttribute('name');
          if (!name) continue;
          const inner = page
            .frameLocator(`iframe.webview[name="${name}"]`)
            .frameLocator('iframe#active-frame');
          const wanted = contains
            ? inner.locator(selector).filter({ hasText: contains })
            : inner.locator(selector);
          if ((await wanted.count()) > 0) {
            found = inner;
            return true;
          }
        }
        return false;
      },
      { message: `a webview containing ${selector}${contains ? ` with ${contains}` : ''}` },
    )
    .toBe(true);
  return found!;
}
