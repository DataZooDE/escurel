import { LitElement, css, html, nothing } from 'lit';
import { property } from 'lit/decorators.js';
import type {
  OverviewItem,
  OverviewTile,
  OverviewTone,
  OverviewView,
  OverviewWebviewToHost,
} from '../../src/shared/protocol';
import { checkIcon, clockIcon, warnIcon } from '../shared/icons';
import { theme } from '../shared/theme.css';

/** A shape per tone, so the board never leans on colour alone: a warning triangle, a check, a clock. */
const toneIcon = (tone: OverviewTone) =>
  tone === 'attention' ? warnIcon() : tone === 'ok' ? checkIcon() : clockIcon();

/**
 * The overview board: what needs a person today, at a glance. Talks to the host only through
 * `escurel-message` events that main.ts forwards, and only ever sends a line's key or a tile id.
 */
export class EscurelOverview extends LitElement {
  static properties = {
    view: { attribute: false },
    error: { type: String },
  };

  @property({ attribute: false }) view: OverviewView | undefined;
  @property() error: string | undefined;

  static styles = [
    theme,
    css`
      :host {
        padding: 20px 28px 40px;
        min-height: 100%;
        box-sizing: border-box;
      }
      header {
        display: flex;
        align-items: baseline;
        gap: 12px;
        flex-wrap: wrap;
        margin-bottom: 16px;
      }
      h1 {
        font-size: 1.5em;
        font-weight: 600;
        margin: 0;
      }
      .updated {
        color: var(--escurel-muted);
      }
      .spacer {
        flex: 1;
      }
      header button {
        border-color: var(--vscode-button-border, var(--escurel-border));
      }
      .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
        gap: 16px;
        align-items: start;
      }
      section.tile {
        border: 1px solid var(--escurel-border);
        border-left-width: 4px;
        border-radius: 4px;
        padding: 12px 14px 10px;
        background: var(--vscode-sideBar-background, transparent);
        min-width: 0;
      }
      section.tone-attention {
        border-left-color: var(--vscode-editorWarning-foreground);
      }
      section.tone-ok {
        border-left-color: var(--escurel-run);
      }
      section.tone-neutral {
        border-left-color: var(--escurel-border);
      }
      h2 {
        margin: 0 0 4px;
        font-size: 0.85em;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
        color: var(--escurel-muted);
      }
      h2 button {
        font: inherit;
        letter-spacing: inherit;
        text-transform: inherit;
        color: inherit;
        padding: 0;
        border: none;
      }
      h2 button:hover {
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
      }
      .headline {
        display: flex;
        align-items: center;
        gap: 6px;
        font-size: 1.15em;
        font-weight: 600;
        margin: 0 0 8px;
      }
      .headline svg {
        width: 14px;
        height: 14px;
        flex: none;
      }
      .tone-attention .headline svg {
        color: var(--vscode-editorWarning-foreground);
      }
      .tone-ok .headline svg {
        color: var(--escurel-run);
      }
      ul {
        list-style: none;
        margin: 0;
        padding: 0;
      }
      li + li {
        border-top: 1px solid var(--escurel-border);
      }
      button.item {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        width: 100%;
        text-align: left;
        padding: 6px 4px;
        border: none;
        border-radius: 2px;
        min-width: 0;
      }
      button.item:hover {
        background: var(--vscode-list-hoverBackground);
      }
      .label {
        max-width: 100%;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .detail,
      .more,
      .empty {
        color: var(--escurel-muted);
        font-size: 0.9em;
      }
      .detail {
        max-width: 100%;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .more,
      .empty {
        margin: 6px 4px 0;
      }
      .error,
      .loading {
        margin-top: 24px;
      }
      .error {
        border: 1px solid var(--vscode-editorWarning-foreground);
        border-radius: 4px;
        padding: 12px 14px;
      }
      .error p {
        margin: 0 0 8px;
      }
    `,
  ];

  private send(message: OverviewWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent('escurel-message', { detail: message, bubbles: true, composed: true }),
    );
  }

  private line(item: OverviewItem) {
    return html`<li>
      <button
        class="item"
        data-key=${item.key}
        title=${item.detail ? `${item.label}\n${item.detail}` : item.label}
        @click=${() => this.send({ type: 'open', key: item.key })}
      >
        <span class="label">${item.label}</span>
        ${item.detail ? html`<span class="detail">${item.detail}</span>` : nothing}
      </button>
    </li>`;
  }

  private tile(t: OverviewTile) {
    return html`<section class="tile tone-${t.tone}" aria-label=${t.title}>
      <h2>
        <button
          title=${`Open ${t.title.toLowerCase()} in full`}
          @click=${() => this.send({ type: 'open-tile', tile: t.id })}
        >
          ${t.title}
        </button>
      </h2>
      <p class="headline">${toneIcon(t.tone)}<span>${t.headline}</span></p>
      ${
        t.items.length
          ? html`<ul>
              ${t.items.map((i) => this.line(i))}
            </ul>`
          : html`<p class="empty">${t.empty}</p>`
      }
      ${t.more ? html`<p class="more">+${t.more} more</p>` : nothing}
    </section>`;
  }

  render() {
    if (this.error) {
      return html`<div class="error" role="alert">
        <p>${this.error}</p>
        <button @click=${() => this.send({ type: 'refresh' })}>Try again</button>
      </div>`;
    }
    const v = this.view;
    if (!v) return html`<p class="loading muted">Loading the overview…</p>`;
    const at = new Date(v.updatedAt);
    const hh = String(at.getHours()).padStart(2, '0');
    const mm = String(at.getMinutes()).padStart(2, '0');
    return html`
      <header>
        <h1>Today</h1>
        <span class="updated">Updated ${hh}:${mm}</span>
        <span class="spacer"></span>
        <button class="refresh" @click=${() => this.send({ type: 'refresh' })}>Refresh</button>
        <button class="focus-toggle" @click=${() => this.send({ type: 'toggle-focus' })}>
          ${v.focusOn ? 'Leave focus view' : 'Switch to focus view'}
        </button>
      </header>
      <div class="grid">${v.tiles.map((t) => this.tile(t))}</div>
    `;
  }
}

customElements.define('escurel-overview', EscurelOverview);
