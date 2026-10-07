import { defineConfig } from '@playwright/test';

// Live end-to-end tests: a real VS Code window with the real extension, a real gateway that
// verifies tokens and a real runner, driven by clicking. The Evolve scenario runs in CI with
// built binaries and an isolated Xvfb display; see fixtures.ts and `npm run test:e2e`.
// ESCUREL_E2E_SLOW=4 stretches every wait for a shared machine that is overloaded by other jobs.
const SLOW = Number(process.env.ESCUREL_E2E_SLOW) || 1;

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  timeout: 180_000 * SLOW,
  expect: { timeout: 30_000 * SLOW },
  // A selector that matches nothing must fail in seconds, not wait out the whole test timeout.
  use: { actionTimeout: 30_000 * SLOW, screenshot: 'only-on-failure' },
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  outputDir: 'artifacts/results',
});
