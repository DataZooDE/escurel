import { css } from 'lit';

// Every colour in a webview is a VS Code theme token (SPEC §1, §2). The four
// nouns keep one semantic colour each, everywhere they appear.
export const theme = css`
  :host {
    --escurel-skill: var(--vscode-charts-purple);
    --escurel-instance: var(--vscode-charts-blue);
    --escurel-event: var(--vscode-charts-orange);
    --escurel-run: var(--vscode-charts-green);
    --escurel-run-failed: var(--vscode-errorForeground);
    color: var(--vscode-foreground);
    font-family: var(--vscode-font-family);
    font-size: var(--vscode-font-size);
    background: var(--vscode-editor-background);
  }
`;
