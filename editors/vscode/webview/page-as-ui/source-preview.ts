import { LitElement, css, html, nothing } from 'lit';
import type { PreviewModel } from '../../src/shared/preview';
import { theme } from '../shared/theme.css';

const isHttp = (s: string | undefined): s is string => !!s && /^https?:\/\/\S+$/i.test(s);

/**
 * What the source system holds for a page whose data is not markdown: SQL rows, live API fields or
 * document chunks, always read-only. Every value is plain text (no markup); a source that failed is an
 * alert, not an empty table. The `open-original` event asks the host for a document's original file.
 */
export class EscurelSourcePreview extends LitElement {
  static properties = { preview: { attribute: false }, resource: {} };
  static styles = [
    theme,
    css`
      :host {
        display: block;
      }
      header {
        display: flex;
        align-items: baseline;
        gap: 8px;
        flex-wrap: wrap;
        margin-bottom: 6px;
      }
      .badge {
        border: 1px solid var(--escurel-border);
        border-radius: 9px;
        padding: 0 8px;
        font-size: 0.85em;
        color: var(--escurel-muted);
      }
      .source,
      .resource {
        color: var(--escurel-muted);
        font-size: 0.9em;
      }
      .scroll {
        overflow-x: auto;
      }
      table {
        border-collapse: collapse;
        min-width: 40%;
      }
      th,
      td {
        border: 1px solid var(--escurel-border);
        padding: 3px 8px;
        text-align: left;
        white-space: nowrap;
      }
      th {
        background: var(--vscode-editor-inactiveSelectionBackground, transparent);
        font-weight: 600;
      }
      dl {
        display: grid;
        grid-template-columns: minmax(120px, 200px) 1fr;
        gap: 4px 12px;
        margin: 0;
      }
      dt {
        color: var(--escurel-muted);
      }
      dd {
        margin: 0;
        white-space: pre-wrap;
        overflow-wrap: anywhere;
      }
      .chunk {
        border-left: 2px solid var(--escurel-border);
        padding: 2px 10px;
        margin: 6px 0;
        white-space: pre-wrap;
      }
      .note,
      .empty {
        color: var(--escurel-muted);
        margin: 6px 0 0;
      }
      [role='alert'] {
        border: 1px solid var(--vscode-editorWarning-foreground);
        padding: 6px 10px;
      }
    `,
  ];

  preview?: PreviewModel;
  resource?: string;

  private openOriginal(): void {
    this.dispatchEvent(new CustomEvent('open-original', { bubbles: true, composed: true }));
  }

  private head(source: string) {
    return html`<header>
      <span class="badge" title="This data lives in the source system; edit it there"
        >read-only (source)</span
      >
      ${source ? html`<span class="source">${source}</span>` : nothing}
      ${
        this.resource
          ? isHttp(this.resource)
            ? html`<a class="resource" href=${this.resource} rel="noopener noreferrer"
                >${this.resource}</a
              >`
            : html`<span class="resource">${this.resource}</span>`
          : nothing
      }
    </header>`;
  }

  override render() {
    const p = this.preview;
    if (!p) return nothing;
    switch (p.kind) {
      case 'rows':
        return html`${this.head(p.source)}${
          p.columns.length === 0
            ? html`<p class="empty">The source returned no rows.</p>`
            : html`<div class="scroll">
                <table aria-label=${`Rows from ${p.source || 'the source'}`}>
                  <thead>
                    <tr>
                      ${p.columns.map((c) => html`<th scope="col">${c}</th>`)}
                    </tr>
                  </thead>
                  <tbody>
                    ${p.rows.map(
                      (r) =>
                        html`<tr>
                          ${r.map((v) => html`<td>${v}</td>`)}
                        </tr>`,
                    )}
                  </tbody>
                </table>
              </div>`
        }${p.truncated ? html`<p class="note">Showing the first ${p.rows.length} rows; more rows exist in the source.</p>` : nothing}`;
      case 'fields':
        return html`${this.head(p.source)}
          <dl>
            ${p.fields.map(
              (f) =>
                html`<dt>${f.name}</dt>
                  <dd>${f.value}</dd>`,
            )}
          </dl>`;
      case 'document':
        return html`${this.head('')}
          ${p.chunks.map((c) => html`<div class="chunk" data-anchor=${c.anchor}>${c.text}</div>`)}
          <p class="note">
            Showing ${p.chunks.length} of ${p.total} chunks.
            <button class="open-original" @click=${this.openOriginal}>Open original</button>
          </p>`;
      case 'issue':
        return html`${this.head(p.source)}
          <div role="alert">
            The source could not be read: ${p.message} <span class="muted">(${p.code})</span>
          </div>`;
    }
  }
}

if (!customElements.get('escurel-source-preview')) {
  customElements.define('escurel-source-preview', EscurelSourcePreview);
}
