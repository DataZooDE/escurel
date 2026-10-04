import { LitElement, css, html, nothing } from 'lit';
import type {
  ActionView,
  InspectorActions,
  InspectorRow,
  InspectorView,
  RunControl,
  RunControlAction,
  StartMode,
  ThreadWebviewToHost,
} from '../../src/shared/protocol';
import { middleTruncate } from '../../src/shared/middleTruncate';
import { START_ITEMS } from '../shared/skill-button';
import '../shared/skill-button';
import { splitButton, theme } from '../shared/theme.css';

export class EscurelThreadInspector extends LitElement {
  static override properties = {
    detail: { attribute: false },
    nodeId: { type: String },
  };

  declare detail: InspectorView | undefined;
  declare nodeId: string | undefined;

  static override styles = [
    theme,
    splitButton,
    css`
      :host {
        display: block;
        padding: 12px 16px;
      }
      .kind {
        display: block;
        color: var(--escurel-muted);
        font-size: 0.85em;
        text-transform: uppercase;
        letter-spacing: 0.04em;
      }
      h2 {
        font-size: 1.2em;
        margin: 2px 0 8px;
      }
      .summary {
        margin: 0 0 12px;
        padding: 8px 10px;
        border-left: 3px solid var(--vscode-focusBorder);
        background: var(--vscode-editorWidget-background, transparent);
      }
      .summary.needs-you {
        border-left-color: var(--vscode-editorWarning-foreground);
      }
      .summary .needs {
        display: inline-block;
        margin-right: 8px;
        padding: 0 8px;
        border: 1px solid var(--vscode-editorWarning-foreground);
        border-radius: 9px;
        color: var(--vscode-editorWarning-foreground);
        font-size: 0.85em;
        font-weight: 600;
      }
      /* A wide, short panel: sections sit side by side instead of one long column. */
      .cols {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
        gap: 4px 28px;
        align-items: start;
      }
      .cols > section {
        margin-top: 8px;
      }
      details.tech {
        margin-top: 14px;
        color: var(--escurel-muted);
      }
      details.tech summary {
        cursor: pointer;
      }
      details.tech summary:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      details.tech dl {
        margin-top: 8px;
      }
      .id {
        font-family: var(--vscode-editor-font-family, monospace);
      }
      button.copy {
        margin-left: 8px;
        padding: 0 6px;
        height: 20px;
        font: inherit;
        font-size: 0.85em;
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        border-radius: 2px;
        cursor: pointer;
      }
      button.copy:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      h3 {
        font-size: 1em;
        margin: 0 0 8px;
      }
      section {
        margin-top: 16px;
      }
      .actions-wrapper {
        margin-bottom: 16px;
        display: flex;
        flex-direction: column;
        gap: 8px;
      }
      .links {
        display: flex;
        flex-wrap: wrap;
        gap: 4px 16px;
        margin: 6px 0 2px;
      }
      .link-button {
        appearance: none;
        background: none;
        border: 0;
        padding: 0;
        font: inherit;
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
        cursor: pointer;
      }
      .link-button:hover {
        color: var(--vscode-textLink-activeForeground);
      }
      .link-button:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      .actions {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
        align-items: center;
      }
      .control-button {
        appearance: none;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        height: 24px;
        padding: 0 10px;
        font-family: inherit;
        font-size: var(--escurel-font-size-ui, 12px);
        /* Secondary like run detail; only Approve plan is the primary action. */
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        border-radius: 2px;
        cursor: pointer;
        user-select: none;
        box-sizing: border-box;
      }
      .control-button:hover:not([aria-disabled='true']) {
        background: var(
          --vscode-button-secondaryHoverBackground,
          var(--vscode-button-hoverBackground)
        );
      }
      .control-button.primary {
        color: var(--vscode-button-foreground);
        background: var(--vscode-button-background);
      }
      .control-button.primary:hover:not([aria-disabled='true']) {
        background: var(--vscode-button-hoverBackground);
      }
      .control-button:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      .control-button[aria-disabled='true'] {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .control-hint {
        flex-basis: 100%;
        margin: 0;
        font-size: 0.9em;
        color: var(--escurel-muted);
      }
      dl {
        display: grid;
        grid-template-columns: minmax(70px, 140px) minmax(0, 1fr);
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

  private send(message: ThreadWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<ThreadWebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  private onStartSkill(skill: string, pageId: string, mode: StartMode): void {
    this.send({ type: 'start-skill', skill, pageId, mode });
  }

  private onSelectSkill(skill: string, pageId: string, id: string): void {
    if (id === 'skill') {
      this.send({ type: 'view-skill', skill });
    } else {
      this.send({ type: 'start-skill', skill, pageId, mode: id as StartMode });
    }
  }

  private onRunControl(action: RunControlAction): void {
    const runId = this.nodeId ?? (this.detail?.actions as { runId?: string } | undefined)?.runId;
    this.send({ type: 'run-control', action, runId });
  }

  private renderSkillActions(skills: { pageId: string; actions: ActionView[] }) {
    return html`
      <div class="actions skills" role="group" aria-label="Skills">
        ${skills.actions.map(
          (a) => html`
            <escurel-split-button
              class="skill-button"
              noun="skill"
              .label=${a.label}
              title=${`Starts skill ${a.skill} with an agent on this page`}
              .header=${`skill ${a.skill}`}
              .items=${START_ITEMS}
              @primary=${() => this.onStartSkill(a.skill, skills.pageId, 'background')}
              @select=${(e: CustomEvent<string>) =>
                this.onSelectSkill(a.skill, skills.pageId, e.detail)}
            ></escurel-split-button>
          `,
        )}
      </div>
    `;
  }

  private renderControlActions(controls: RunControl[]) {
    // A tooltip reaches neither keyboard, touch nor screen reader, so the reason a control is
    // deactivated is on the page as text and the button points at it.
    const reason = controls.find((c) => !c.enabled && c.disabledReason)?.disabledReason;
    return html`
      <div class="actions controls" role="group" aria-label="Run controls">
        ${controls.map(
          (c) => html`
            <button
              class="control-button ${c.action === 'approve' ? 'primary' : ''}"
              aria-disabled=${c.enabled ? nothing : 'true'}
              title=${c.disabledReason ?? c.label}
              aria-describedby=${!c.enabled && c.disabledReason ? 'control-hint' : nothing}
              @click=${() => c.enabled && this.onRunControl(c.action)}
            >
              ${c.label}
            </button>
          `,
        )}
        ${reason ? html`<p class="control-hint" id="control-hint">${reason}</p>` : nothing}
      </div>
    `;
  }

  private renderActions(actions?: InspectorActions) {
    if (!actions) return nothing;
    const hasSkills = Boolean(actions.skills?.actions?.length);
    const hasControls = Boolean(actions.controls?.length);
    if (!hasSkills && !hasControls) return nothing;

    return html`
      <div class="actions-wrapper">
        ${hasSkills ? this.renderSkillActions(actions.skills!) : nothing}
        ${hasControls ? this.renderControlActions(actions.controls!) : nothing}
      </div>
    `;
  }

  private copy(key: string, value: string): void {
    // A webview may refuse the clipboard; the full value is always in the tooltip too.
    void navigator.clipboard?.writeText(value).catch(() => undefined);
    this.dispatchEvent(
      new CustomEvent('escurel-copied', { detail: key, bubbles: true, composed: true }),
    );
  }

  private renderValue(row: InspectorRow) {
    // Identifiers are cut in the middle (both ends are what a person compares); the whole value
    // is the tooltip and the Copy button's payload.
    const long = row.tech && row.v.length > 24;
    if (!long) return html`${row.v}`;
    return html`<span class="id" title=${row.v}>${middleTruncate(row.v, 24)}</span
      ><button
        type="button"
        class="copy"
        aria-label=${`Copy ${row.k}`}
        @click=${() => this.copy(row.k, row.v)}
      >
        Copy
      </button>`;
  }

  private renderRows(rows: InspectorRow[]) {
    return html`<dl>
      ${rows.map(
        (row) =>
          html`<dt>${row.k}</dt>
            <dd class=${row.tone ? `tone-${row.tone}` : ''}>${this.renderValue(row)}</dd>`,
      )}
    </dl>`;
  }

  protected override render() {
    const detail = this.detail;
    if (!detail) return nothing;
    const plain = detail.rows.filter((r) => !r.tech);
    const tech = detail.rows.filter((r) => r.tech);
    return html`
      ${detail.kindLabel ? html`<span class="kind">${detail.kindLabel}</span>` : nothing}
      <h2>${detail.title}</h2>
      ${
        detail.summary
          ? html`<p class="summary ${detail.needsYou ? 'needs-you' : ''}">
              ${detail.needsYou ? html`<span class="needs">Needs you</span>` : nothing}${detail.summary}
            </p>`
          : nothing
      }
      ${
        detail.links?.length
          ? html`<nav class="links" aria-label="Go to">
              ${detail.links.map(
                (l) =>
                  html`<button
                    class="link-button"
                    data-link=${l.id}
                    @click=${() => this.send({ type: 'open-link', nodeId: this.nodeId ?? '', link: l.id })}
                  >
                    ${l.label}
                  </button>`,
              )}
            </nav>`
          : nothing
      }
      ${this.renderActions(detail.actions)}
      <div class="cols">
        ${plain.length ? html`<section>${this.renderRows(plain)}</section>` : nothing}
        ${
          detail.side.length
            ? html`<section class="side">
                <h3>${detail.sideTitle}</h3>
                ${this.renderRows(detail.side)}
              </section>`
            : nothing
        }
        ${
          detail.body
            ? html`<section>
                <h3>${detail.bodyTitle}</h3>
                <div class="body">${detail.body}</div>
              </section>`
            : nothing
        }
      </div>
      ${
        tech.length
          ? html`<details class="tech">
              <summary>Technical details</summary>
              ${this.renderRows(tech)}
            </details>`
          : nothing
      }
    `;
  }
}

if (!customElements.get('escurel-thread-inspector')) {
  customElements.define('escurel-thread-inspector', EscurelThreadInspector);
}
