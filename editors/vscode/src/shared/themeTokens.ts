/**
 * The theme tokens every Escurel webview resolves, as data. `webview/shared/theme.css.ts` declares the
 * same names inside shadow roots (a test keeps the two in step); a companion's webview gets them on
 * `:root` through `webviewHtml`. Values are VS Code theme variables only: no literal colours.
 */
export const THEME_TOKENS: Readonly<Record<string, string>> = Object.freeze({
  '--escurel-skill': 'var(--vscode-charts-purple)',
  '--escurel-instance': 'var(--vscode-charts-blue)',
  '--escurel-event': 'var(--vscode-charts-orange)',
  '--escurel-run': 'var(--vscode-charts-green)',
  '--escurel-run-failed': 'var(--vscode-errorForeground)',
  '--escurel-border':
    'var(--vscode-contrastBorder, var(--vscode-widget-border, var(--vscode-panel-border)))',
  '--escurel-muted': 'var(--vscode-descriptionForeground)',
});

export const THEME_CSS = `:root{${Object.entries(THEME_TOKENS)
  .map(([k, v]) => `${k}:${v}`)
  .join(';')}}`;
