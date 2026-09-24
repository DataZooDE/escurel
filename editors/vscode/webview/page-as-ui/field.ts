import { LitElement, html, nothing } from 'lit';
import { property } from 'lit/decorators.js';
import type { FieldView } from '../../src/shared/protocol';
import './split-button';

/** One typed field row (SPEC §3.4), rendered by `render` then `kind`; read-only until PR-1. */
export class EscurelField extends LitElement {
  static properties = { field: { attribute: false }, editable: { type: Boolean } };

  @property({ attribute: false }) field!: FieldView;
  @property({ type: Boolean }) editable = false;

  protected override createRenderRoot() {
    return this;
  }

  private emit(detail: unknown, type = 'escurel-message'): void {
    this.dispatchEvent(new CustomEvent(type, { detail, bubbles: true, composed: true }));
  }

  private value() {
    const f = this.field;
    if (f.kind === 'link' && f.link) {
      const { skill, pageId } = f.link;
      return html`<escurel-split-button
        class="instance-button"
        noun="instance"
        .label=${f.display}
        title="Open instance"
        .header=${`skill ${skill}`}
        .items=${[
          { id: 'open', label: `Open instance — ${f.display}` },
          { id: 'skill', label: `View skill — ${skill}` },
        ]}
        @primary=${() => this.emit({ type: 'open-page', pageId })}
        @select=${(e: CustomEvent<string>) => this.emit(e.detail === 'open' ? { type: 'open-page', pageId } : { type: 'view-skill', skill })}
      ></escurel-split-button>`;
    }
    switch (f.render) {
      case 'badge':
        return html`<span class="badge">${f.display}</span>`;
      case 'markdown':
        return html`<div class="markdown">${f.display}</div>`;
    }
    switch (f.kind) {
      case 'bool':
        return html`<input
          type="checkbox"
          .checked=${f.value === true}
          ?disabled=${!this.editable}
        />`;
      case 'int':
      case 'float':
      case 'date':
      case 'datetime':
      case 'string':
      case 'enum':
      default:
        return html`<span class="value">${f.display}</span>`;
    }
  }

  override render() {
    const f = this.field;
    return html`<div class="field" data-name=${f.name} data-kind=${f.kind} data-render=${f.render}>
      <span class="name"
        >${f.label}${f.required ? html`<span class="required" title="required"> *</span>` : nothing}</span
      >
      <span>${this.value()}</span>
    </div>`;
  }
}
customElements.define('escurel-field', EscurelField);
