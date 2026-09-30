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
  browsers: [playwrightLauncher({ product: 'chromium' })],
};
