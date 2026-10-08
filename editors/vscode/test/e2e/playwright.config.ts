import { defineConfig } from '@playwright/test';

// Live end-to-end tests: a real VS Code window with the real extension, a real gateway that
// verifies tokens and a real runner, driven by clicking. The Evolve scenario runs in CI with
// built binaries and an isolated Xvfb display; see fixtures.ts and `npm run test:e2e`.
export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  timeout: 180_000,
  expect: { timeout: 30_000 },
  // A selector that matches nothing must fail in seconds, not wait out the whole test timeout.
  use: { actionTimeout: 30_000, screenshot: 'only-on-failure' },
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  outputDir: 'artifacts/results',
});
