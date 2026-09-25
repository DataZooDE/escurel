import { LitElement, css, html, nothing } from 'lit';
import { property, state } from 'lit/decorators.js';
import type { PageModel, StartMode, WebviewToHost } from '../../src/shared/protocol';
import { fieldRows, splitButton, theme } from '../shared/theme.css';
import './field';
import './split-button';

const START_ITEMS = [
  { id: 'background', label: 'Start in background' },
  { id: 'plan', label: 'First make a plan' },
  { id: 'terminal', label: 'Start in terminal' },
  { id: 'skill', label: 'View skill' },
];

/**
 * Page as UI (SPEC §3.4): header with the Skill link, the typed field
 * form, summary, body, the gate state, the skill's actions as Skill split
 * buttons and the Page | Markdown toggle. Read-only until BACKEND_GAPS
 * PR-1. Talks to the host only through `escurel-message` events that
 * main.ts forwards over postMessage.
 */
export class EscurelPageAsUi extends LitElement {
  static styles = [
    theme,
    splitButton,
    fieldRows,
    css`
      :host {
        padding: 12px 20px 40px;
      }
      header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 12px;
        flex-wrap: wrap;
      }
      .toggle {
        display: inline-flex;
        border: 1px solid var(--escurel-border);
        border-radius: 2px;
      }
      .toggle button[aria-pressed='true'] {
        background: var(--vscode-button-background);
        color: var(--vscode-button-foreground);
      }
      h1 {
        font-size: 1.5em;
        margin: 8px 0 2px;
      }
      .subline,
      .skill-row {
        color: var(--escurel-muted);
        font-size: 0.9em;
      }
      .skill-link {
        color: var(--escurel-skill);
        font-weight: 600;
        padding: 0 2px;
      }
      section {
        margin-top: 18px;
      }
      h2 {
        font-size: 1em;
        margin: 0 0 6px;
        display: flex;
        gap: 8px;
        align-items: baseline;
      }
      .gate {
        display: inline-flex;
        gap: 6px;
        align-items: center;
        padding: 4px 8px;
        border-radius: 2px;
        border: 1px solid var(--vscode-editorWarning-foreground);
        color: var(--vscode-editorWarning-foreground);
      }
      .gate.auto {
        border-color: var(--escurel-run);
        color: var(--escurel-run);
      }
      .summary {
        white-space: pre-wrap;
      }
      .body {
        white-space: pre-wrap;
        font-family: var(--vscode-editor-font-family);
        border: 1px solid var(--escurel-border);
        border-radius: 2px;
        padding: 8px 12px;
      }
      .actions {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
      }
      .status {
        padding: 24px;
        color: var(--escurel-muted);
      }
      .error {
        color: var(--vscode-errorForeground);
      }
    `,
  ];
  static properties = {
    model: { attribute: false },
    error: { attribute: false },
    view: { state: true },
  };

  @property({ attribute: false }) model?: PageModel;
  @property({ attribute: false }) error?: string;
  @state() private view: 'page' | 'markdown' = 'page';

  private send(message: WebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<WebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  private showRaw(): void {
    this.view = 'markdown';
    this.send({ type: 'show-raw' });
  }

  private start(skill: string, id: string): void {
    if (id === 'skill') return this.send({ type: 'view-skill', skill });
    this.send({ type: 'start-skill', skill, mode: id as StartMode });
  }

  override render() {
    if (this.error) return html`<div class="status error">${this.error}</div>`;
    const m = this.model;
    if (!m) return html`<div class="status">Loading…</div>`;
    const gate = m.skill.autonomy;
    return html`
      <header>
        <span class="toggle" role="group" aria-label="View">
          <button
            aria-pressed=${this.view === 'page' ? 'true' : 'false'}
            @click=${() => (this.view = 'page')}
          >
            Page
          </button>
          <button
            aria-pressed=${this.view === 'markdown' ? 'true' : 'false'}
            @click=${this.showRaw}
          >
            Markdown
          </button>
        </span>
        <span class="gate ${gate}" title="autonomy: ${gate}"
          >${gate === 'auto' ? '● no human gate' : `⚠ human gate: ${gate}`}${m.skill.readOnly ? html` · <span class="chip">${m.skill.layer} · read-only</span>` : nothing}</span
        >
      </header>
      <h1>${m.title}</h1>
      <div class="subline">
        page id ${m.pageId} · backend
        ${m.skill.backend}${m.lastWrittenBy ? html` · last written by ${m.lastWrittenBy}` : nothing}
      </div>
      <div class="skill-row">
        skill
        <button
          class="skill-link"
          title="View skill"
          @click=${() => this.send({ type: 'view-skill', skill: m.skill.id })}
        >
          ${m.skill.id}
        </button>
        — ${m.skill.summary ?? m.skill.description}
      </div>

      <section class="fields">
        ${m.fields.map((f) => html`<escurel-field .field=${f} ?editable=${m.editable}></escurel-field>`)}
        <p class="muted">
          ${m.editable ? 'Editing a field updates your live draft.' : 'Editing arrives with live personal drafts (backend PR-1); until then this form is read-only.'}
        </p>
      </section>

      ${
        m.summary
          ? html`<section>
              <h2>Summary <span class="muted">agent-written</span></h2>
              <div class="summary">${m.summary}</div>
            </section>`
          : nothing
      }

      <section>
        <h2>Body</h2>
        <div class="body">${m.body}</div>
      </section>

      ${
        m.actions.length
          ? html`<section>
              <h2>Follow-ups <span class="muted">declared by skill ${m.skill.id}</span></h2>
              <div class="actions">
                ${m.actions.map(
                  (a) =>
                    html`<escurel-split-button
                      class="skill-button"
                      noun="skill"
                      .label=${a.label}
                      title=${`Starts skill ${a.skill} with an agent on this page`}
                      .header=${`skill ${a.skill}`}
                      .items=${START_ITEMS}
                      @primary=${() => this.start(a.skill, 'background')}
                      @select=${(e: CustomEvent<string>) => this.start(a.skill, e.detail)}
                    ></escurel-split-button>`,
                )}
              </div>
            </section>`
          : nothing
      }
    `;
  }
}
customElements.define('escurel-page-as-ui', EscurelPageAsUi);
