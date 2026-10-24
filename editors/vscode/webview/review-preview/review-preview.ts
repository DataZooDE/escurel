import { LitElement, css, html } from 'lit';
import { property } from 'lit/decorators.js';
import type { PageAugmentView } from '../../src/shared/augment';
import type { ReviewPreviewWebviewToHost } from '../../src/shared/protocol';
import '../shared/augment-view';
import { theme } from '../shared/theme.css';

/**
 * What companion hooks add beside a review diff, drawn from the declarative schema. Read-only: a
 * preview never offers a form, whatever the host sent. It talks to the host only through
 * `escurel-message` events carrying a handle.
 */
export class EscurelReviewPreview extends LitElement {
  static styles = [
    theme,
    css`
      :host {
        padding: 12px 20px 40px;
      }
      .status {
        color: var(--escurel-muted);
      }
    `,
  ];

  @property({ attribute: false }) view?: PageAugmentView;

  private send(message: ReviewPreviewWebviewToHost): void {
    this.dispatchEvent(new CustomEvent('escurel-message', { detail: message, bubbles: true, composed: true }));
  }

  override render() {
    if (!this.view) return html`<div class="status">Loading…</div>`;
    return html`<escurel-augment
      .view=${{ ...this.view, forms: [], actions: [] }}
      @augment-action=${(e: CustomEvent<{ id: string }>) =>
        this.send({ type: 'augment-action', id: e.detail.id })}
    ></escurel-augment>`;
  }
}
customElements.define('escurel-review-preview', EscurelReviewPreview);
