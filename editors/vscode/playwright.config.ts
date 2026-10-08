import { defineConfig } from '@playwright/test';

// The three VS Code theme kinds and the Escurel Calm theme are injected as --vscode-* token sheets by
// the harness; one project per kind so screenshots are compared per theme.
export default defineConfig({
  testDir: 'test/visual',
  snapshotDir: 'test/visual/__screenshots__',
  snapshotPathTemplate: '{snapshotDir}/{projectName}/{testFilePath}/{arg}{ext}',
  webServer: {
    command: 'node scripts/serve-static.mjs',
    port: 4173,
    reuseExistingServer: !process.env.CI,
  },
  use: { baseURL: 'http://127.0.0.1:4173', viewport: { width: 900, height: 600 } },
  projects: [
    { name: 'light', metadata: { theme: 'light' } },
    { name: 'dark', metadata: { theme: 'dark' } },
    { name: 'high-contrast', metadata: { theme: 'high-contrast' } },
    // The theme the calm focus view applies (themes/escurel-calm-color-theme.json).
    { name: 'calm', metadata: { theme: 'calm' } },
  ],
});
