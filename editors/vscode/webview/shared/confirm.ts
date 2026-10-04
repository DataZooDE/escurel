import { css, html } from 'lit';
import type { Confirmation } from '../../src/runs/controlWording';

/** The inline question asked before something that cannot be taken back; a webview has no modal. */
export const confirmStyles = css`
  .confirm {
    flex-basis: 100%;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid var(--vscode-editorWarning-foreground);
    border-left-width: 4px;
    border-radius: 2px;
  }
  .confirm .detail {
    color: var(--escurel-muted);
    flex-basis: 100%;
  }
  .confirm button.danger {
    background: var(--vscode-button-background);
    color: var(--vscode-button-foreground);
  }
`;

export function confirmRow(c: Confirmation, onConfirm: () => void, onKeep: () => void) {
  return html`<div class="confirm" role="alertdialog" aria-label=${c.message}>
    <strong>${c.message}</strong>
    <button class="danger confirm-yes" @click=${onConfirm}>${c.button}</button>
    <button class="confirm-no" @click=${onKeep}>Keep it</button>
    <span class="detail">${c.detail}</span>
  </div>`;
}
