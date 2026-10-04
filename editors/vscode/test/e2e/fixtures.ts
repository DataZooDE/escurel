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
  display: string;
  /** What the demo driver left behind: the root events and changesets of the story. */
  story: { rootA: string; rootB: string; promoted: string; awaiting: string };
  gatewayUrl: string;
  /** Call a gateway tool as the signed-in human (alice), for setting up or checking state. */
  call: (name: string, args: Record<string, unknown>, admin?: boolean) => Promise<ToolResult>;
  /** Console and page errors collected since the window opened. */
  errors: string[];
  shot: (name: string) => Promise<void>;
}

export const test = base.extend<object, { stack: Stack }>({
  stack: [
    // eslint-disable-next-line no-empty-pattern -- Playwright requires the fixtures argument to be a destructuring pattern.
    async ({}, use) => {
      const home = mkdtempSync(join(homedir(), '.cache', 'escurel-e2e-'));
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
          '--ozone-platform=x11 --disable-site-isolation-trials --disable-features=IsolateOrigins,site-per-process',
        ESCUREL_TEST_GATEWAY_BIN: join(bin, 'escurel-test-gateway'),
        ESCUREL_RUNNER_BIN: join(bin, 'escurel-runner'),
        ESCUREL_ECHO_SLEEP_MS: '6000',
        // No zoom: Playwright maps clicks into a nested webview with the page's own scale, and a zoomed
        // window (the demo's default) puts them on the wrong element.
        ESCUREL_DEMO_ZOOM: '0',
        ESCUREL_DEMO_EVOLVE_SEED: '1',
      };
      const run = join(EXT, 'demo', 'run.sh');
      execFileSync(run, ['start'], { env, stdio: 'inherit', timeout: 240_000 });

      const info = JSON.parse(readFileSync(join(home, 'gateway.json'), 'utf8').split('\n')[0]!);
      const story = JSON.parse(readFileSync(join(home, 'story.json'), 'utf8'));
      const bearer = () =>
        JSON.parse(readFileSync(join(home, 'bearer.json'), 'utf8')) as {
          bearer: string;
          admin_bearer: string;
        };

      let browser: Browser | undefined;
      for (let i = 0; i < 60 && !browser; i += 1) {
        try {
          browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
        } catch {
          await new Promise((r) => setTimeout(r, 500));
        }
      }
      if (!browser) throw new Error('could not attach to the VS Code window');
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
      };
      try {
        await use(stack);
      } finally {
        await browser.close().catch(() => undefined);
        try {
          execFileSync(run, ['stop'], { env, stdio: 'ignore' });
        } catch {
          /* already gone */
        }
        xvfb.kill();
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
