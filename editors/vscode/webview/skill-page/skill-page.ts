import { LitElement, css, html, nothing } from 'lit';
import { property } from 'lit/decorators.js';
import type { SkillPageModel, SkillPageToHost } from '../../src/shared/skillPage';
import { formatAge } from '../../src/shared/time';
import { lockIcon } from '../shared/icons';
import { theme } from '../shared/theme.css';

/**
 * The readable page of a skill: what it is for, its fields, what it can start, the instances it has and
 * its recent runs. The Markdown file stays one click away (Show Markdown): it is the source, not the page
 * a person reads. Talks to the host only through `escurel-message` events that main.ts forwards.
 */
export class EscurelSkillPage extends LitElement {
  static properties = {
    model: { attribute: false },
    error: { type: String },
  };

  @property({ attribute: false }) model: SkillPageModel | undefined;
  @property() error: string | undefined;

  static styles = [
    theme,
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
      h1 {
        font-size: 1.5em;
        margin: 8px 0 2px;
      }
      h2 {
        font-size: 1em;
        margin: 0 0 6px;
        display: flex;
        gap: 8px;
        align-items: baseline;
      }
      section {
        margin-top: 18px;
      }
      .kind {
        color: var(--escurel-skill);
        font-weight: 600;
      }
      .lede {
        margin: 2px 0 0;
      }
      .stale-badge {
        border: 1px solid var(--vscode-editorWarning-foreground);
        color: var(--vscode-editorWarning-foreground);
        border-radius: 3px;
        padding: 0 5px;
        margin-right: 6px;
        font-weight: 600;
      }
      .provenance {
        color: var(--escurel-muted);
        font-size: 0.85em;
        margin-top: 4px;
      }
      dl.facts {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: 2px 14px;
        margin: 0;
      }
      dl.facts dt {
        color: var(--escurel-muted);
      }
      dl.facts dd {
        margin: 0;
      }
      table {
        border-collapse: collapse;
        width: 100%;
      }
      th,
      td {
        text-align: left;
        padding: 3px 12px 3px 0;
        vertical-align: top;
        border-bottom: 1px solid var(--escurel-border);
      }
      th {
        color: var(--escurel-muted);
        font-weight: 600;
      }
      ul.rows {
        list-style: none;
        padding: 0;
        margin: 0;
      }
      ul.rows li {
        display: flex;
        gap: 10px;
        align-items: baseline;
        padding: 2px 0;
      }
      .link {
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
        padding: 0;
        border: none;
      }
      .state {
        font-size: 0.85em;
        border: 1px solid var(--escurel-border);
        border-radius: 9px;
        padding: 0 6px;
      }
      .state.waiting {
        color: var(--escurel-event);
      }
      .state.done {
        color: var(--escurel-run);
      }
      .actions {
        display: flex;
        gap: 8px;
        flex-wrap: wrap;
      }
      .actions button {
        border-color: var(--escurel-skill);
      }
      .status.error {
        color: var(--vscode-errorForeground);
      }
    `,
  ];

  private send(message: SkillPageToHost): void {
    this.dispatchEvent(
      new CustomEvent<SkillPageToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  override render() {
    if (this.error) return html`<div class="status error">${this.error}</div>`;
    const m = this.model;
    if (!m) return html`<div class="status">Loading…</div>`;
    return html`
      <header>
        <span class="kind">Skill</span>
        <button
          class="show-markdown"
          title="Open the skill's Markdown source"
          @click=${() => this.send({ type: 'show-raw' })}
        >
          Show Markdown
        </button>
      </header>
      <h1>${m.title}</h1>
      <p class="lede">${m.summary ?? m.description}</p>
      ${m.summary && m.description !== m.summary ? html`<p class="muted">${m.description}</p>` : nothing}
      ${
        m.provenance.length
          ? html`<div class="provenance">
              ${m.stale ? html`<span class="stale-badge">Stale</span>` : nothing}
              ${m.provenance.filter((f) => f !== 'stale').join(' · ')}
            </div>`
          : nothing
      }

      <section class="about">
        <h2>
          About ${m.readOnly ? html`<span class="chip">${lockIcon()} read-only</span>` : nothing}
        </h2>
        <dl class="facts">
          ${m.facts.map(
            (f) =>
              html`<dt>${f.label}</dt>
                <dd>${f.value}</dd>`,
          )}
        </dl>
      </section>

      <section class="fields">
        <h2>Fields <span class="muted">what each ${m.id} record holds</span></h2>
        ${
          m.fields.length
            ? html`<table>
                <thead>
                  <tr>
                    <th>Field</th>
                    <th>Needed</th>
                    <th>Holds</th>
                  </tr>
                </thead>
                <tbody>
                  ${m.fields.map(
                    (f) =>
                      html`<tr>
                        <td>
                          ${f.label}${f.description ? html`<div class="muted">${f.description}</div>` : nothing}
                        </td>
                        <td>${f.required ? 'required' : 'optional'}</td>
                        <td>${f.detail}</td>
                      </tr>`,
                  )}
                </tbody>
              </table>`
            : html`<p class="muted">This skill declares no fields.</p>`
        }
      </section>

      ${
        m.actions.length
          ? html`<section class="follow-ups">
              <h2>
                What it can start <span class="muted">follow-ups declared by this skill</span>
              </h2>
              <div class="actions">
                ${m.actions.map(
                  (a) =>
                    html`<button
                      title=${`Starts skill ${a.skill}; you pick the record it works on`}
                      @click=${() => this.send({ type: 'start-skill', skill: a.skill, mode: 'run' })}
                    >
                      ${a.label}
                    </button>`,
                )}
              </div>
            </section>`
          : nothing
      }

      <section class="instances">
        <h2>
          Records
          <span class="muted"
            >first ${m.instances.items.length}${m.instances.more ? ', more in Knowledge' : ''}</span
          >
        </h2>
        ${
          m.instances.items.length
            ? html`<ul class="rows">
                ${m.instances.items.map(
                  (i) =>
                    html`<li>
                      <button
                        class="link"
                        @click=${() => this.send({ type: 'open-page', pageId: i.pageId })}
                      >
                        ${i.title}
                      </button>
                    </li>`,
                )}
              </ul>`
            : html`<p class="muted">No records yet.</p>`
        }
      </section>

      <section class="runs">
        <h2>Recent runs <span class="muted">latest events for this skill</span></h2>
        ${
          m.runs.length
            ? html`<ul class="rows">
                ${m.runs.map(
                  (r) =>
                    html`<li>
                      <span class="state ${r.state}">${r.state}</span>
                      <button
                        class="link"
                        @click=${() => this.send({ type: 'open-thread', rootEventId: r.rootEventId })}
                      >
                        ${r.title}
                      </button>
                      ${
                        r.runId
                          ? html`<button
                              class="link open-run"
                              @click=${() => this.send({ type: 'open-run', runId: r.runId! })}
                            >
                              Open run
                            </button>`
                          : nothing
                      }
                      <span class="muted">${formatAge(r.at)}</span>
                    </li>`,
                )}
              </ul>`
            : html`<p class="muted">No runs yet.</p>`
        }
      </section>
    `;
  }
}

customElements.define('escurel-skill-page', EscurelSkillPage);
