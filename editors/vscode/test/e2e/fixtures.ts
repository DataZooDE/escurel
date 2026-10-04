import {
  test as base,
  expect,
  chromium,
  type Browser,
  type FrameLocator,
  type Page,
} from '@playwright/test';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync, readFileSync, mkdirSync } from 'node:fs';
import { createServer } from 'node:net';
import { createServer as createHttpServer, type Server as HttpServer } from 'node:http';
import { tmpdir } from 'node:os';
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
  display: string;
  /** What the demo driver left behind: the root events and changesets of the story. */
  story: { rootA: string; rootB: string; promoted: string; awaiting: string };
  gatewayUrl: string;
  /** Call a gateway tool as the signed-in human (alice), for setting up or checking state. */
  call: (name: string, args: Record<string, unknown>, admin?: boolean) => Promise<ToolResult>;
  /** Console and page errors collected since the window opened. */
  errors: string[];
  shot: (name: string) => Promise<void>;
  setGeminiPlanTarget: (pageId: string, revision: string) => void;
  geminiRequests: Record<string, unknown>[];
}

export const test = base.extend<object, {
  stack: Stack;
  runnerHarness: 'echo' | 'gemini';
}>({
  runnerHarness: ['echo', { scope: 'worker', option: true }],
  stack: [
    async ({ runnerHarness }, use) => {
      const home = mkdtempSync(join(tmpdir(), 'escurel-e2e-'));
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
              if (!firstRequest.includes(geminiPlanTarget.pageId) ||
                  !firstRequest.includes(geminiPlanTarget.revision)) {
                response.writeHead(422).end('Evolve plan prompt omitted the frozen problem page');
                return;
              }
              parts = [{ functionCall: { name: 'expand', args: {
                page_id: geminiPlanTarget.pageId, raw: true,
              } } }];
            } else if (contents?.length === 3) {
              parts = [{ functionCall: { name: 'report_progress', args: { plan: [
                { step: 'Review the frozen source, holdout and V2 budget', status: 'pending' },
                { step: 'Run bounded DuckDB search after owner approval', status: 'pending' },
              ] } } }];
            } else {
              parts = [{ text: 'Plan reported for the owner to review.' }];
            }
          }
          response.setHeader('content-type', 'application/json');
          response.end(JSON.stringify({ candidates: [{ content: { parts } }] }));
        });
        await new Promise<void>((resolveListen) =>
          modelServer!.listen(0, '127.0.0.1', resolveListen));
        const address = modelServer.address();
        if (!address || typeof address === 'string') throw new Error('Gemini stub has no port');
        modelBase = `http://127.0.0.1:${address.port}`;
      }
      const artifacts = resolve(__dirname, 'artifacts');
      mkdirSync(artifacts, { recursive: true });
      const display = `:${90 + Math.floor(Math.random() * 9)}`;
      const xvfb: ChildProcess = spawn('Xvfb', [display, '-screen', '0', '1700x1000x24'], {
        stdio: 'ignore',
      });
      await new Promise((r) => setTimeout(r, 1200));
      const cdpPort = await freePort();
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
          (process.env.CI ? ' --no-sandbox' : ''),
        ESCUREL_TEST_GATEWAY_BIN: join(bin, 'escurel-test-gateway'),
        ESCUREL_RUNNER_BIN: join(bin, 'escurel-runner'),
        ESCUREL_ECHO_SLEEP_MS: '6000',
        // No zoom: Playwright maps clicks into a nested webview with the page's own scale, and a zoomed
        // window (the demo's default) puts them on the wrong element.
        ESCUREL_DEMO_ZOOM: '0',
        // Keep VS Code's modal confirmation inside the CDP window so the
        // approval text and deliberate human click are observable end to end.
        ESCUREL_DEMO_DIALOG_STYLE: 'custom',
        ESCUREL_DEMO_EVOLVE_SEED: '1',
        ESCUREL_DEMO_RUNNER_HARNESS: runnerHarness,
        ...(modelBase ? {
          ESCUREL_GEMINI_API_KEY: 'deterministic-native-plan-key',
          ESCUREL_RUNNER_GEMINI_BASE_URL: modelBase,
        } : {}),
      };
      const run = join(EXT, 'demo', 'run.sh');
      let browser: Browser | undefined;
      try {
        execFileSync(run, ['start'], { env, stdio: 'inherit', timeout: 240_000 });

      const info = JSON.parse(readFileSync(join(home, 'gateway.json'), 'utf8').split('\n')[0]!);
      const story = JSON.parse(readFileSync(join(home, 'story.json'), 'utf8'));
      const bearer = () =>
        JSON.parse(readFileSync(join(home, 'bearer.json'), 'utf8')) as {
          bearer: string;
          admin_bearer: string;
        };

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
      await page.waitForSelector('.monaco-workbench', { timeout: 60_000 });
      const errors: string[] = [];
      page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
      page.on('console', (m) => {
        if (m.type() === 'error') errors.push(`console: ${m.text()}`);
      });

      const stack: Stack = {
        page,
        display,
        story,
        gatewayUrl: info.gateway_url,
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
        setGeminiPlanTarget: (pageId, revision) => { geminiPlanTarget = { pageId, revision }; },
        geminiRequests,
      };
      await use(stack);
      } finally {
        await browser?.close().catch(() => undefined);
        try {
          execFileSync(run, ['stop'], { env, stdio: 'ignore' });
        } catch {
          /* already gone */
        }
        xvfb.kill();
        if (modelServer) await new Promise<void>((resolveClose) => modelServer!.close(() => resolveClose()));
      }
    },
    { scope: 'worker', timeout: 300_000 },
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
export async function webviewWith(page: Page, selector: string): Promise<FrameLocator> {
  const outer = page.locator('iframe.webview:visible');
  let found: FrameLocator | undefined;
  await expect
    .poll(
      async () => {
        const n = await outer.count();
        for (let i = 0; i < n; i += 1) {
          const inner = page
            .frameLocator('iframe.webview:visible')
            .nth(i)
            .frameLocator('iframe#active-frame');
          if ((await inner.locator(selector).count()) > 0) {
            found = inner;
            return true;
          }
        }
        return false;
      },
      { message: `a webview containing ${selector}` },
    )
    .toBe(true);
  return found!;
}
