import { LitElement, css, html, nothing } from 'lit';
import type { InspectorRow, InspectorView } from '../../src/shared/protocol';
import { theme } from '../shared/theme.css';

export class EscurelThreadInspector extends LitElement {
  static override properties = { detail: { attribute: false } };
  declare detail: InspectorView | undefined;

  static override styles = [
    theme,
    css`
      :host {
        padding: 12px 16px;
      }
      h2 {
        font-size: 1.2em;
        margin: 0 0 12px;
      }
      h3 {
        font-size: 1em;
        margin: 0 0 8px;
      }
      section {
        margin-top: 16px;
      }
      dl {
        display: grid;
        grid-template-columns: minmax(90px, 1fr) minmax(0, 2fr);
        gap: 6px 12px;
        margin: 0;
      }
      dt {
        color: var(--escurel-muted);
      }
      dd {
        margin: 0;
        overflow-wrap: anywhere;
      }
      .tone-ok {
        color: var(--vscode-charts-green);
      }
      .tone-warn {
        color: var(--vscode-editorWarning-foreground);
      }
      .tone-error {
        color: var(--vscode-errorForeground);
      }
      .body {
        white-space: pre-wrap;
        overflow-wrap: anywhere;
      }
    `,
  ];

  private renderRows(rows: InspectorRow[]) {
    return html`<dl>
      ${rows.map(
        (row) =>
          html`<dt>${row.k}</dt>
            <dd class=${row.tone ? `tone-${row.tone}` : ''}>${row.v}</dd>`,
      )}
    </dl>`;
  }

  protected override render() {
    const detail = this.detail;
    if (!detail) return nothing;
    return html`
      <h2>${detail.title}</h2>
      ${this.renderRows(detail.rows)}
      ${
        detail.body
          ? html`<section>
              <h3>${detail.bodyTitle}</h3>
              <div class="body">${detail.body}</div>
            </section>`
          : nothing
      }
      ${
        detail.side.length
          ? html`<section class="side">
              <h3>${detail.sideTitle}</h3>
              ${this.renderRows(detail.side)}
            </section>`
          : nothing
      }
    `;
  }
}
if (!customElements.get('escurel-thread-inspector')) {
  customElements.define('escurel-thread-inspector', EscurelThreadInspector);
}
