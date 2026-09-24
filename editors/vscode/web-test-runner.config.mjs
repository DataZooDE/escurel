import { esbuildPlugin } from '@web/dev-server-esbuild';
import { playwrightLauncher } from '@web/test-runner-playwright';

export default {
  files: 'test/component/**/*.test.ts',
  nodeResolve: true,
  plugins: [esbuildPlugin({ ts: true, target: 'es2022', tsconfig: 'tsconfig.webview.json' })],
  browsers: [playwrightLauncher({ product: 'chromium' })],
};
