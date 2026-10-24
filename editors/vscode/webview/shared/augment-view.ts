import { LitElement, css, html, nothing } from 'lit';
import { property, state } from 'lit/decorators.js';
import type {
  AugmentBlock,
  AugmentFieldView,
  AugmentFormView,
  AugmentTone,
  PageAugmentView,
} from '../../src/shared/augment';
import { theme } from './theme.css';

const TONE_WORDS: Record<AugmentTone, string> = {
  attention: 'needs attention',
  ok: 'ok',
  neutral: '',
};

/**
 * What a companion extension adds to a core view, drawn from the declarative schema (sections,
 * buttons, forms). Everything is a text node; no companion script runs here. It talks to its host
 * only through `augment-action` and `augment-form` events, which carry handles and values, never a
 * command or a tool name.
 */
export class EscurelAugmentView extends LitElement {
  static styles = [
    theme,
    css`
      :host {
        background: transparent;
      }
      section.augment-section {
        margin: 12px 0;
      }
      h2 {
        font-size: 1.1em;
        margin: 0 0 6px;
      }
      .augment-text,
      .augment-empty,
      .augment-unavailable,
      .form-hint {
        color: var(--escurel-muted);
      }
      table {
        border-collapse: collapse;
        width: 100%;
      }
      th,
      td {
        text-align: left;
        padding: 2px 8px 2px 0;
        border-bottom: 1px solid var(--escurel-border);
      }
      th {
        color: var(--escurel-muted);
        font-weight: normal;
      }
      .facts {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: 2px 12px;
        margin: 0;
      }
      .facts dt {
        color: var(--escurel-muted);
      }
      .facts dd {
        margin: 0;
      }
      .tone {
        margin-left: 6px;
        border: 1px solid currentColor;
        border-radius: 2px;
        padding: 0 4px;
        font-size: 0.9em;
      }
      .tone.attention {
        color: var(--vscode-editorWarning-foreground);
      }
      ul.list {
        margin: 0;
        padding-left: 18px;
      }
      .detail {
        color: var(--escurel-muted);
        margin-left: 6px;
      }
      .augment-actions {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
        margin: 8px 0;
      }
      button.primary,
      form button[type='submit'] {
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
      }
      button.link {
        color: var(--vscode-textLink-foreground);
        padding: 0;
        border: none;
      }
      form {
        border: 1px solid var(--escurel-border);
        border-radius: 2px;
        padding: 8px 12px;
        margin: 8px 0;
        display: grid;
        gap: 8px;
      }
      form h3 {
        margin: 0;
        font-size: 1em;
      }
      label {
        display: grid;
        gap: 2px;
      }
      textarea,
      input[type='text'],
      select {
        font: inherit;
        color: var(--vscode-input-foreground);
        background: var(--vscode-input-background);
        border: 1px solid var(--vscode-input-border, var(--escurel-border));
        padding: 2px 4px;
      }
      .field-error,
      .form-result.problem {
        color: var(--vscode-errorForeground);
      }
    `,
  ];

  @property({ attribute: false }) view?: PageAugmentView;
  /** What became of the last submit (host answer). */
  @property({ attribute: false }) result?: { id: string; ok: boolean; message: string };
  @state() private values: Record<string, Record<string, unknown>> = {};
  @state() private errors: Record<string, string> = {};

  private emit<T>(name: string, detail: T): void {
    this.dispatchEvent(new CustomEvent(name, { detail, bubbles: true, composed: true }));
  }

  private setValue(formId: string, name: string, value: unknown): void {
    this.values = { ...this.values, [formId]: { ...this.values[formId], [name]: value } };
  }

  private submit(e: Event, f: AugmentFormView): void {
    e.preventDefault();
    const given = this.values[f.id] ?? {};
    const values: Record<string, unknown> = {};
    for (const field of f.fields) {
      const v = given[field.name];
      if (field.kind === 'bool') values[field.name] = v === true;
      else values[field.name] = typeof v === 'string' ? v : '';
    }
    const missing = f.fields.find(
      (x) => x.required && x.kind !== 'bool' && !String(values[x.name]).trim(),
    );
    if (missing) {
      this.errors = { ...this.errors, [f.id]: `${missing.label} is required.` };
      return;
    }
    this.errors = { ...this.errors, [f.id]: '' };
    this.emit('augment-form', { id: f.id, values });
  }

  private block(b: AugmentBlock) {
    switch (b.type) {
      case 'text':
        return html`<p class="augment-text">${b.text}</p>`;
      case 'facts':
        return html`<dl class="facts">
          ${b.rows.map(
            (r) =>
              html`<dt>${r.label}</dt>
                <dd>
                  ${r.value}${r.tone && TONE_WORDS[r.tone]
                    ? html`<span class="tone ${r.tone}">${TONE_WORDS[r.tone]}</span>`
                    : nothing}
                </dd>`,
          )}
        </dl>`;
      case 'table':
        return html`<table>
          <thead>
            <tr>
              ${b.columns.map((c) => html`<th scope="col">${c}</th>`)}
            </tr>
          </thead>
          <tbody>
            ${b.rows.map(
              (r) => html`<tr>
                ${r.map((c, i) => html`<td data-label=${b.columns[i] ?? ''}>${c}</td>`)}
              </tr>`,
            )}
          </tbody>
        </table>`;
      case 'list':
        return html`<ul class="list">
          ${b.items.map(
            (i) =>
              html`<li>
                ${i.key
                  ? html`<button class="link" @click=${() => this.emit('augment-action', { id: i.key })}>
                      ${i.label}
                    </button>`
                  : html`<span>${i.label}</span>`}
                ${i.detail ? html`<span class="detail">${i.detail}</span>` : nothing}
                ${i.tone && TONE_WORDS[i.tone]
                  ? html`<span class="tone ${i.tone}">${TONE_WORDS[i.tone]}</span>`
                  : nothing}
              </li>`,
          )}
        </ul>`;
    }
  }

  private field(f: AugmentFormView, field: AugmentFieldView) {
    const current = this.values[f.id]?.[field.name];
    const label = html`${field.label}${field.required ? html` <span aria-hidden="true">*</span>` : nothing}`;
    switch (field.kind) {
      case 'textarea':
        return html`<label
          >${label}<textarea
            rows="3"
            maxlength=${field.maxLength}
            .value=${typeof current === 'string' ? current : ''}
            @input=${(e: Event) => this.setValue(f.id, field.name, (e.target as HTMLTextAreaElement).value)}
          ></textarea
        ></label>`;
      case 'enum':
        return html`<label
          >${label}<select
            @change=${(e: Event) => this.setValue(f.id, field.name, (e.target as HTMLSelectElement).value)}
          >
            <option value="" ?selected=${!current}></option>
            ${field.values?.map((v) => html`<option value=${v} ?selected=${current === v}>${v}</option>`)}
          </select></label
        >`;
      case 'bool':
        return html`<label
          ><span
            ><input
              type="checkbox"
              .checked=${current === true}
              @change=${(e: Event) => this.setValue(f.id, field.name, (e.target as HTMLInputElement).checked)}
            />
            ${field.label}</span
          ></label
        >`;
      default:
        return html`<label
          >${label}<input
            type="text"
            maxlength=${field.maxLength}
            .value=${typeof current === 'string' ? current : ''}
            @input=${(e: Event) => this.setValue(f.id, field.name, (e.target as HTMLInputElement).value)}
        /></label>`;
    }
  }

  private form(f: AugmentFormView) {
    const r = this.result?.id === f.id ? this.result : undefined;
    return html`<form @submit=${(e: Event) => this.submit(e, f)} aria-label=${f.title}>
      <h3>${f.title}</h3>
      ${f.fields.map((field) => this.field(f, field))}
      ${this.errors[f.id] ? html`<div class="field-error" role="alert">${this.errors[f.id]}</div>` : nothing}
      <div><button type="submit">${f.submitLabel}</button></div>
      <p class="form-hint">Creates a proposal that a reviewer decides; nothing changes until then.</p>
      ${r ? html`<div class="form-result ${r.ok ? '' : 'problem'}" role="status">${r.message}</div>` : nothing}
    </form>`;
  }

  override render() {
    const v = this.view;
    if (!v) return nothing;
    const any = v.sections.length || v.actions.length || v.forms.length || v.unavailable;
    if (!any) return nothing;
    return html`
      ${v.sections.map(
        (s) => html`<section class="augment-section" data-section=${s.id}>
          <h2>${s.title}</h2>
          ${s.blocks.length
            ? s.blocks.map((b) => this.block(b))
            : html`<p class="augment-empty">${s.empty ?? 'Nothing to show.'}</p>`}
        </section>`,
      )}
      ${v.actions.length
        ? html`<div class="augment-actions">
            ${v.actions.map(
              (a) =>
                html`<button
                  class="primary"
                  title=${a.hint ?? a.label}
                  @click=${() => this.emit('augment-action', { id: a.id })}
                >
                  ${a.label}
                </button>`,
            )}
          </div>`
        : nothing}
      ${v.forms.map((f) => this.form(f))}
      ${v.unavailable
        ? html`<p class="augment-unavailable" role="status">
            Some additional information could not be read just now.
          </p>`
        : nothing}
    `;
  }
}
customElements.define('escurel-augment', EscurelAugmentView);
