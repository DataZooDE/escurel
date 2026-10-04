import { isSourceField } from '../../src/shared/rowSource';
import { sourceBanner } from '../../src/shared/sourceBanner';
import { writeBackLine } from '../../src/shared/writeBack';
import { writeBackLead } from '../../src/shared/writeBackLead';
import { checkIcon, lockIcon, syncIcon, warnIcon } from '../shared/icons';
import { markdownStyles, renderMarkdown } from '../shared/markdown-view';
import { LitElement, css, html, nothing } from 'lit';
import { property, state } from 'lit/decorators.js';
import type { PageModel, StartMode, WebviewToHost } from '../../src/shared/protocol';
import { fieldRows, splitButton, theme } from '../shared/theme.css';
import './field';
import './source-preview';
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
    markdownStyles,
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
        /* Weight and an inset ring, so the selection never depends on a fill (high contrast has none). */
        font-weight: 600;
        box-shadow: inset 0 0 0 2px var(--vscode-contrastActiveBorder, transparent);
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
      .skill-facts {
        color: var(--escurel-muted);
        font-size: 0.85em;
        margin-top: 2px;
      }
      .stale-badge {
        border: 1px solid var(--vscode-editorWarning-foreground);
        color: var(--vscode-editorWarning-foreground);
        border-radius: 3px;
        padding: 0 5px;
        margin-right: 6px;
        font-weight: 600;
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
      .source-strip .head {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 8px;
      }
      .source-strip .head .spacer {
        flex: 1;
      }
      .source-strip .notes {
        margin-top: 4px;
        color: var(--escurel-muted);
      }
      .source-strip .chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        border: 1px solid var(--vscode-editorInfo-foreground, var(--escurel-border));
        border-radius: 9px;
        padding: 0 8px;
        white-space: nowrap;
        font-size: 0.9em;
      }
      .source-strip svg.lock {
        flex: none;
      }
      .source-strip button.retry,
      .source-strip button.add-note {
        padding: 1px 8px;
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        border-radius: 2px;
        cursor: pointer;
      }
      .write-back .lead {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        flex: none;
        font-weight: 700;
      }
      .write-back .lead svg {
        align-self: center;
      }
      details.page-meta {
        margin: 2px 0 6px;
        color: var(--escurel-muted);
        font-size: 0.9em;
      }
      details.page-meta summary {
        cursor: pointer;
      }
      h2 .lock {
        vertical-align: -1px;
        margin-right: 4px;
      }
      .source-strip {
        margin: 8px 0;
        padding: 6px 10px;
        border: 1px solid var(--escurel-border);
        border-left: 3px solid var(--vscode-textLink-foreground);
        border-radius: 3px;
        color: var(--vscode-foreground);
        background: var(--vscode-textBlockQuote-background, transparent);
      }
      .source-strip.problem {
        border-left-color: var(--vscode-editorWarning-foreground);
      }
      .source-strip .issue {
        display: block;
        color: var(--vscode-editorWarning-foreground);
      }
      /* Data that came from an outside system: marked, and never styled as the page's own. */
      .source-strip .external {
        border: 1px solid var(--vscode-editorInfo-foreground, var(--escurel-border));
        border-radius: 3px;
        padding: 0 5px;
        white-space: nowrap;
      }
      .source-strip button.propose {
        margin-left: 8px;
        padding: 1px 8px;
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        border-radius: 2px;
        cursor: pointer;
      }
      .write-back {
        display: flex;
        align-items: baseline;
        gap: 8px;
        margin: 0 0 8px;
        padding: 4px 10px;
        border-left: 3px solid var(--vscode-textLink-foreground);
        color: var(--vscode-foreground);
      }
      .write-back.problem {
        border-left-color: var(--vscode-editorWarning-foreground);
        color: var(--vscode-editorWarning-foreground);
      }
      /* A column of the source row: a quiet accent on its label, and the words for a screen reader. */
      .field[data-source='true'] .name {
        border-left: 2px solid var(--vscode-textLink-foreground);
        padding-left: 6px;
      }
      .visually-hidden {
        position: absolute;
        width: 1px;
        height: 1px;
        overflow: hidden;
        clip-path: inset(50%);
        white-space: nowrap;
      }
      .thread-strip {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 6px;
        margin: 8px 0;
        padding: 4px 8px;
        border: 1px solid var(--vscode-widget-border, var(--vscode-contrastBorder, transparent));
        border-radius: 3px;
        color: var(--vscode-descriptionForeground);
      }
      .thread-strip button {
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
      }
      .run-status.failed,
      .run-status.dead_letter,
      .run-status.cancelled {
        color: var(--vscode-errorForeground);
      }

      .summary {
        white-space: pre-wrap;
      }
      .body {
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

  override connectedCallback(): void {
    super.connectedCallback();
    // A wikilink anywhere in the page (body, a markdown field) bubbles up to here.
    this.addEventListener('escurel-wikilink', (e) => this.onWikilink(e));
  }

  private onWikilink(e: Event): void {
    this.send({ type: 'open-wikilink', wikilink: (e as CustomEvent<string>).detail });
  }

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

  /** What a row of an `instances: rows` skill is: read-only data from a source, plus the person's notes. */
  private sourceStrip(source: NonNullable<PageModel['source']>) {
    const b = sourceBanner(source);
    return html`<div class="source-strip ${b.problem ? 'problem' : ''}" role="note">
      <div class="head">
        <strong>${lockIcon()} ${b.headline}</strong>
        ${b.chips.map(
          (c) => html`<span class="chip external" title=${c.title}>${lockIcon()} ${c.label}</span>`,
        )}
        <span class="spacer"></span>
        ${
          b.notes.action
            ? html`<button
                class="add-note"
                title="Open the Markdown tab, where your notes live"
                @click=${() => this.showRaw()}
              >
                ${b.notes.action}
              </button>`
            : nothing
        }
        ${(source.writableColumns ?? []).map(
          (field) =>
            html`<button
              class="propose"
              title="Propose a change to ${field} in the source. A reviewer approves it before the source is touched."
              @click=${() => this.send({ type: 'propose-write-back', field })}
            >
              Change ${field}…
            </button>`,
        )}
      </div>
      <div class="notes">${b.notes.text}</div>
      ${
        b.issue
          ? html`<div class="issue" title=${b.issue.detail}>
              ${b.issue.text}
              ${
                b.issue.retry
                  ? html`<button class="retry" @click=${() => this.send({ type: 'refresh' })}>
                      Retry
                    </button>`
                  : nothing
              }
            </div>`
          : nothing
      }
    </div>`;
  }

  /** What the last change sent to the source did (from its `escurel:write-back` events). */
  private writeBackLine(status: NonNullable<PageModel['writeBack']>) {
    const bad =
      status.outcome === 'failed' || status.outcome === 'rejected' || status.outcome === 'conflict';
    const lead = writeBackLead(status.outcome);
    const icon =
      lead.tone === 'ok' ? checkIcon() : lead.tone === 'pending' ? syncIcon() : warnIcon();
    return html`<div class="write-back ${bad ? 'problem' : ''}" role="status">
      <span class="lead">${icon}<span>${lead.word}</span></span
      >${writeBackLine(status)}
    </div>`;
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
        ${m.lastWrittenBy ? html`Last written by ${m.lastWrittenBy}` : nothing}
      </div>
      <details class="page-meta">
        <summary>Page details</summary>
        <div>page id ${m.pageId} · backend ${m.skill.backend}</div>
      </details>
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
      ${
        m.skill.facts?.length
          ? html`<div class="skill-facts">
              ${m.skill.stale ? html`<span class="stale-badge">Stale</span>` : nothing}
              ${m.skill.facts.filter((f) => f !== 'stale').join(' · ')}
            </div>`
          : nothing
      }
      ${
        m.thread
          ? html`<div class="thread-strip">
              <span>Thread</span>
              <button
                class="open-thread"
                aria-label="Open thread for this page's last run"
                @click=${() => this.send({ type: 'open-thread', rootEventId: m.thread!.rootEventId })}
              >
                Open thread
              </button>
              <span aria-hidden="true">→</span>
              <button
                class="open-run"
                title=${m.thread.runId}
                @click=${() => this.send({ type: 'open-run', runId: m.thread!.runId })}
              >
                Open run
              </button>
              <span class="run-status ${m.thread.runStatus}"
                >${m.thread.runStatus.replaceAll('_', ' ')}</span
              >
              <span aria-hidden="true">→</span>
              <span>this page</span>
            </div>`
          : nothing
      }
      ${m.source ? this.sourceStrip(m.source) : nothing}
      ${m.writeBack ? this.writeBackLine(m.writeBack) : nothing}

      <section class="fields">
        ${m.fields.map((f) => html`<escurel-field .field=${f} ?editable=${m.editable} ?source=${isSourceField(m.source, f)}></escurel-field>`)}
        ${
          m.source
            ? nothing
            : html`<p class="muted readonly-note">
                ${
                  m.editable
                    ? 'Editing a field updates your live draft.'
                    : 'This form is read-only. Switch to Markdown to edit; a save is held as your draft until you promote it.'
                }
              </p>`
        }
      </section>

      ${
        m.preview
          ? html`<section>
              <h2>${lockIcon()}Source data · read-only</h2>
              <escurel-source-preview
                .preview=${m.preview}
                .resource=${m.resource}
                @open-original=${() => this.send({ type: 'open-original' })}
              ></escurel-source-preview>
            </section>`
          : nothing
      }
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
        <div class="body md">${renderMarkdown(m.body)}</div>
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
