import js from '@eslint/js';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import lit from 'eslint-plugin-lit';
import noHexColour from './eslint/no-hex-colour.js';

export default tseslint.config(
  {
    ignores: [
      'dist/**',
      'node_modules/**',
      '.vscode-test/**',
      'docs/mock/**',
      'test/visual/__screenshots__/**',
      'test-results/**',
      'playwright-report/**',
      '*.vsix',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: [
      'src/**/*.ts',
      'test/unit/**/*.ts',
      'test/integration/**/*.ts',
      'test/visual/**/*.ts',
      'scripts/**',
      'demo/**',
      'eslint/**',
      '*.mjs',
      '*.ts',
    ],
    languageOptions: { globals: { ...globals.node, ...globals.mocha } },
  },
  {
    files: ['webview/**/*.ts', 'test/component/**/*.ts'],
    languageOptions: { globals: { ...globals.browser, ...globals.mocha } },
  },
  {
    // The demo's bootstrap is a plain CommonJS extension: VS Code loads it with `require`.
    files: ['demo/bootstrap/**'],
    rules: { '@typescript-eslint/no-require-imports': 'off' },
  },
  {
    // chai's `expect(x).to.exist` is an expression by design.
    files: ['test/component/**/*.ts'],
    rules: { '@typescript-eslint/no-unused-expressions': 'off' },
  },
  {
    files: ['webview/**/*.ts'],
    plugins: { lit, escurel: { rules: { 'no-hex-colour': noHexColour } } },
    rules: { ...lit.configs.recommended.rules, 'escurel/no-hex-colour': 'error' },
  },
  {
    files: ['**/*.ts'],
    rules: {
      '@typescript-eslint/consistent-type-imports': 'error',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
    },
  },
);
