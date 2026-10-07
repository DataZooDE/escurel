import { esbuildPlugin } from '@web/dev-server-esbuild';
import { playwrightLauncher } from '@web/test-runner-playwright';

export default {
  files: 'test/component/**/*.test.ts',
  nodeResolve: true,
  // `json: true` so a component fixture can import a RECORDED payload rather than
  // a hand-copied one; a copy drifts from the wire, the recording does not.
  plugins: [
    esbuildPlugin({ ts: true, json: true, target: 'es2022', tsconfig: 'tsconfig.webview.json' }),
  ],
  // On a machine where Playwright's own Chromium never delivers an animation frame (seen with a wedged
  // GPU driver: `requestAnimationFrame` callbacks never run, so every test that awaits one times out),
  // ESCUREL_WTR_CHROME points the runner at an installed Chrome. Unset, nothing changes: CI uses the
  // bundled browser.
  // A machine under heavy load (load average 60: animation frames arrive late) needs more than mocha's 2 s
  // per test; ESCUREL_WTR_TIMEOUT_MS raises it. Unset, the default stays.
  ...(process.env.ESCUREL_WTR_TIMEOUT_MS
    ? { testFramework: { config: { timeout: Number(process.env.ESCUREL_WTR_TIMEOUT_MS) } } }
    : {}),
  // ... and the whole run may take longer than the runner's default 120 s (ESCUREL_WTR_FINISH_MS).
  ...(process.env.ESCUREL_WTR_FINISH_MS
    ? { testsFinishTimeout: Number(process.env.ESCUREL_WTR_FINISH_MS) }
    : {}),
  browsers: [
    playwrightLauncher({
      product: 'chromium',
      ...(process.env.ESCUREL_WTR_CHROME
        ? { launchOptions: { executablePath: process.env.ESCUREL_WTR_CHROME } }
        : {}),
    }),
  ],
};
