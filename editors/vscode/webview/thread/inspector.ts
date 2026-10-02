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
      .actions-wrapper {
        margin-bottom: 16px;
        display: flex;
        flex-direction: column;
        gap: 8px;
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
      .control-button:hover:not(:disabled) {
        background: var(
          --vscode-button-secondaryHoverBackground,
          var(--vscode-button-hoverBackground)
        );
      }
      .control-button.primary {
        color: var(--vscode-button-foreground);
        background: var(--vscode-button-background);
      }
      .control-button.primary:hover:not(:disabled) {
        background: var(--vscode-button-hoverBackground);
      }
      .control-button:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      .control-button:disabled {
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
      <div class="actions skills" role="toolbar" aria-label="Skills">
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
      <div class="actions controls" role="toolbar" aria-label="Run controls">
        ${controls.map(
          (c) => html`
            <button
              class="control-button ${c.action === 'approve' ? 'primary' : ''}"
              ?disabled=${!c.enabled}
              title=${c.disabledReason ?? c.label}
              aria-describedby=${!c.enabled && c.disabledReason ? 'control-hint' : nothing}
              @click=${() => this.onRunControl(c.action)}
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
      ${this.renderActions(detail.actions)} ${this.renderRows(detail.rows)}
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
