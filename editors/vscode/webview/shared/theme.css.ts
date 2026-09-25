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
    --escurel-border: var(--vscode-widget-border, var(--vscode-panel-border));
    --escurel-muted: var(--vscode-descriptionForeground);
    color: var(--vscode-foreground);
    font-family: var(--vscode-font-family);
    font-size: var(--vscode-font-size);
    background: var(--vscode-editor-background);
    display: block;
  }
  button {
    font: inherit;
    color: inherit;
    background: none;
    border: 1px solid transparent;
    border-radius: 2px;
    padding: 2px 8px;
    cursor: pointer;
  }
  button:focus-visible {
    outline: 1px solid var(--vscode-focusBorder);
    outline-offset: 1px;
  }
  .chip {
    display: inline-block;
    padding: 0 6px;
    border-radius: 9px;
    font-size: 0.85em;
    line-height: 1.6;
    background: var(--vscode-badge-background);
    color: var(--vscode-badge-foreground);
  }
  .muted {
    color: var(--escurel-muted);
  }
`;

/** A split button: a primary segment plus a chevron opening a menu; coloured per noun. */
export const splitButton = css`
  .split {
    position: relative;
    display: inline-flex;
    vertical-align: middle;
  }
  .split .primary,
  .split .chevron {
    border: 1px solid transparent;
    padding: 2px 10px;
    color: var(--vscode-button-foreground);
  }
  .split .primary {
    border-radius: 2px 0 0 2px;
  }
  .split .chevron {
    border-radius: 0 2px 2px 0;
    padding: 2px 6px;
    border-left: 1px solid var(--vscode-editor-background);
  }
  .skill-button .primary,
  .skill-button .chevron {
    background: var(--escurel-skill);
  }
  .instance-button .primary {
    background: var(--vscode-button-secondaryBackground);
    color: var(--escurel-instance);
    font-weight: 600;
  }
  .instance-button .chevron {
    background: var(--vscode-button-secondaryBackground);
    color: var(--escurel-skill);
  }
  .split [role='menu'] {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 5;
    min-width: 240px;
    margin: 0;
    padding: 4px 0;
    list-style: none;
    background: var(--vscode-menu-background, var(--vscode-editorWidget-background));
    color: var(--vscode-menu-foreground, var(--vscode-foreground));
    border: 1px solid var(--vscode-menu-border, var(--escurel-border));
    box-shadow: 0 2px 8px var(--vscode-widget-shadow);
  }
  .split [role='menuitem'] {
    display: block;
    width: 100%;
    text-align: left;
    padding: 4px 12px;
    border: 0;
    border-radius: 0;
    color: inherit;
  }
  .split [role='menuitem']:hover,
  .split [role='menuitem']:focus-visible {
    background: var(--vscode-menu-selectionBackground, var(--vscode-list-hoverBackground));
    color: var(--vscode-menu-selectionForeground, inherit);
    outline: none;
  }
  .split .menu-header {
    padding: 2px 12px 6px;
    font-size: 0.85em;
    color: var(--escurel-muted);
  }
`;

/** The typed field rows of page-as-UI (light DOM, styled by the page). */
export const fieldRows = css`
  escurel-field {
    display: contents;
  }
  .field {
    display: grid;
    grid-template-columns: minmax(120px, 180px) 1fr;
    gap: 4px 12px;
    align-items: center;
    padding: 4px 0;
    border-bottom: 1px solid var(--escurel-border);
  }
  .name {
    color: var(--escurel-muted);
  }
  .required {
    color: var(--vscode-editorWarning-foreground);
  }
  .badge {
    border: 1px solid var(--escurel-border);
    border-radius: 9px;
    padding: 0 8px;
  }
  .markdown {
    white-space: pre-wrap;
    font-family: var(--vscode-editor-font-family);
  }
  input,
  textarea {
    font: inherit;
    color: var(--vscode-input-foreground);
    background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, var(--escurel-border));
    border-radius: 2px;
    padding: 2px 6px;
  }
  input:disabled,
  textarea:disabled {
    opacity: 0.8;
  }
`;
