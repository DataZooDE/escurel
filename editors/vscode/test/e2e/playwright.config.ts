import { defineConfig } from '@playwright/test';

// Live end-to-end tests: a real VS Code window with the real extension, a real gateway that
// verifies tokens and a real runner, driven by clicking. Not run in Docker or in CI (they need the
// built binaries and a display); see the header of fixtures.ts and `npm run test:e2e`.
export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  timeout: 180_000,
  expect: { timeout: 30_000 },
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  outputDir: 'artifacts/results',
});
