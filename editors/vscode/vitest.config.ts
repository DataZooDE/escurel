import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  test: {
    include: ['test/unit/**/*.test.ts'],
    environment: 'node',
  },
  resolve: {
    alias: { vscode: fileURLToPath(new URL('./test/unit/mocks/vscode.ts', import.meta.url)) },
  },
});
