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
  browsers: [
    playwrightLauncher({
      product: 'chromium',
      ...(process.env.ESCUREL_WTR_CHROME
        ? { launchOptions: { executablePath: process.env.ESCUREL_WTR_CHROME } }
        : {}),
    }),
  ],
};
