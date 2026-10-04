import { renderMarkdown } from '../shared/markdown-view';
import { LitElement, html, nothing } from 'lit';
import { property } from 'lit/decorators.js';
import type { FieldView } from '../../src/shared/protocol';
import './split-button';

/** One typed field row (SPEC §3.4), rendered by `render` then `kind`; read-only until PR-1. */
export class EscurelField extends LitElement {
  static properties = {
    field: { attribute: false },
    editable: { type: Boolean },
    source: { type: Boolean },
  };

  @property({ attribute: false }) field!: FieldView;
  @property({ type: Boolean }) editable = false;
  /** This field is a column of the read-only source row (an `instances: rows` skill). */
  @property({ type: Boolean }) source = false;

  protected override createRenderRoot() {
    return this;
  }

  private emit(detail: unknown, type = 'escurel-message'): void {
    this.dispatchEvent(new CustomEvent(type, { detail, bubbles: true, composed: true }));
  }

  private instanceButton(link: { skill: string; id: string; wikilink: string }) {
    const { skill, wikilink, id } = link;
    return html`<escurel-split-button
      class="instance-button"
      noun="instance"
      .label=${id}
      title="Open instance"
      .header=${`skill ${skill}`}
      .items=${[
        { id: 'open', label: `Open instance — ${id}` },
        { id: 'skill', label: `View skill — ${skill}` },
      ]}
      @primary=${() => this.emit({ type: 'open-wikilink', wikilink })}
      @select=${(e: CustomEvent<string>) =>
        this.emit(
          e.detail === 'open' ? { type: 'open-wikilink', wikilink } : { type: 'view-skill', skill },
        )}
    ></escurel-split-button>`;
  }

  private value() {
    const f = this.field;
    // A source value the source did not give: a dash that says so, not a blank that looks broken.
    if (this.source && (f.value === null || f.value === undefined || f.display === '')) {
      return html`<span class="value unavailable" title="The source did not give a value"
        >— <span class="muted">unavailable</span></span
      >`;
    }
    if (f.links?.length)
      return html`<span class="links">${f.links.map((l) => this.instanceButton(l))}</span>`;
    switch (f.render) {
      case 'badge':
        return html`<span class="badge">${f.display}</span>`;
      case 'markdown':
        return html`<div class="markdown md">${renderMarkdown(f.display)}</div>`;
    }
    switch (f.kind) {
      case 'bool':
        // Read-only yes/no is words: a disabled checkbox looks like a bug in light themes.
        if (!this.editable)
          return html`<span class="value">${f.value === true ? 'Yes' : 'No'}</span>`;
        return html`<input type="checkbox" aria-label=${f.label} .checked=${f.value === true} />`;
      default:
        return html`<span class="value">${f.display}</span>`;
    }
  }

  override render() {
    const f = this.field;
    return html`<div
      class="field"
      data-name=${f.name}
      data-kind=${f.kind}
      data-render=${f.render}
      data-source=${this.source ? 'true' : nothing}
    >
      <span class="name"
        >${f.label}${f.required ? html`<span class="required" title="required"> *</span>` : nothing}${
          this.source
            ? html`<span class="source-tag" title="A column of the source row: read-only"
                ><span class="visually-hidden"> (source column, read-only)</span></span
              >`
            : nothing
        }</span
      >
      <span>${this.value()}</span>
    </div>`;
  }
}
customElements.define('escurel-field', EscurelField);
