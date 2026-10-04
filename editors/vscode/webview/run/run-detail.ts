import { emptyAttempts, emptyPlan, runByline, statusIconName } from '../../src/runs/runWording';
import { checkIcon, crossIcon, syncIcon, warnIcon } from '../shared/icons';
import { displayStepStatus, runHeading } from '../../src/runs/runTitle';
import { LitElement, css, html, nothing } from 'lit';
import type { PropertyValues } from 'lit';
import { property, state } from 'lit/decorators.js';
import type { RunControl, RunView, RunWebviewToHost } from '../../src/shared/protocol';
import { formatDateTime, formatDuration } from '../../src/shared/time';
import { theme } from '../shared/theme.css';

const glyphs = { completed: '✓', in_progress: '◐', pending: '○', blocked: '!', unfinished: '◌' };

const statusIcon = (status: string) => {
  switch (statusIconName(status)) {
    case 'check':
      return checkIcon();
    case 'sync':
      return syncIcon();
    case 'cross':
      return crossIcon();
    default:
      return warnIcon();
  }
};

export class EscurelRunDetail extends LitElement {
  static styles = [
    theme,
    css`
      :host {
        padding: 12px 20px 40px;
      }
      h1 {
        font-size: 1.5em;
        margin: 8px 0;
      }
      h2 {
        font-size: 1em;
        margin: 0 0 8px;
      }
      section {
        margin-top: 20px;
      }
      header,
      .meta,
      .trace,
      .controls {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
      }
      .meta {
        color: var(--escurel-muted);
      }
      .controls {
        margin-left: auto;
      }
      .run-control {
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        padding: 4px 10px;
        cursor: pointer;
      }
      .run-control:hover:not([aria-disabled='true']) {
        background: var(--vscode-button-secondaryHoverBackground);
      }
      .run-control.primary {
        color: var(--vscode-button-foreground);
        background: var(--vscode-button-background);
      }
      .run-control.primary:hover:not([aria-disabled='true']) {
        background: var(--vscode-button-hoverBackground);
      }
      .run-control:focus-visible {
        outline: 2px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      .run-control[aria-disabled='true'] {
        opacity: 0.55;
        cursor: not-allowed;
      }
      .control-hint {
        flex-basis: 100%;
        margin: 0;
        text-align: right;
        font-size: 0.9em;
        color: var(--escurel-muted);
      }
      /* An outlined chip on the page's own background: the badge fill made green text about 2.5:1 in
         light themes. The colour is mixed with the foreground so it stays legible in every theme, and
         an icon (a shape per state) says it without colour. */
      .status-chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        background: transparent;
        border: 1px solid currentColor;
        color: color-mix(in srgb, var(--escurel-run) 60%, var(--vscode-foreground));
      }
      .status-chip.failed {
        color: color-mix(in srgb, var(--escurel-run-failed) 70%, var(--vscode-foreground));
      }
      .status-chip.neutral {
        color: var(--vscode-foreground);
      }
      .copy-run {
        background: none;
        border: 0;
        padding: 0;
        font: inherit;
        cursor: pointer;
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
      }
      .copy-run:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      .link,
      .copy-trace {
        /* Buttons that read as the links they are, not as boxed default buttons. */
        background: none;
        border: 0;
        padding: 0;
        font: inherit;
        cursor: pointer;
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
      }
      .link:focus-visible,
      .copy-trace:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      .link:hover,
      .copy-trace:hover {
        color: var(--vscode-textLink-activeForeground);
      }
      .attempt,
      .plan-step,
      .tool-call {
        padding: 8px 0;
        border-bottom: 1px solid var(--escurel-border);
      }
      .attempt-line,
      .tool-call {
        display: flex;
        flex-wrap: wrap;
        gap: 6px;
        align-items: baseline;
      }
      .attempt-error,
      .error,
      .call-error {
        color: var(--vscode-errorForeground);
      }
      .attempt-error {
        margin-top: 4px;
      }
      .plan-step {
        display: flex;
        align-items: baseline;
        gap: 8px;
      }
      .plan-step.blocked {
        color: var(--vscode-editorWarning-foreground);
        font-weight: 600;
      }
      .plan-step .glyph {
        width: 1.2em;
        text-align: center;
      }
      .plan-step .step-status {
        color: var(--escurel-muted);
      }
      .plan-step.blocked .step-status {
        color: inherit;
      }
      .plan-step.unfinished {
        color: var(--escurel-muted);
      }
      .run-id {
        color: var(--escurel-muted);
        font-size: 0.8em;
        font-weight: normal;
      }
      .summary {
        white-space: pre-wrap;
      }
      .status-message {
        padding: 24px 0;
      }
      .reconnect,
      .load-more {
        color: var(--vscode-button-foreground);
        background: var(--vscode-button-background);
      }
      .reconnect:hover,
      .load-more:hover {
        background: var(--vscode-button-hoverBackground);
      }
    `,
  ];

  @property({ attribute: false }) view?: RunView;
  @property({ attribute: false }) error?: { message: string; canReconnect: boolean };
  private loadingMore = false;
  /**
   * A control was sent and the run has not been reported again. The host acts on the click and the
   * run updates by itself a moment later; until then a second click would send the same control
   * twice (two retries, two approvals). Released when the next run state arrives, or after a few
   * seconds in case it never does.
   */
  @state() private controlPending = false;
  private pendingTimer?: ReturnType<typeof setTimeout>;

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has('view')) {
      this.loadingMore = false;
      this.releaseControls();
    }
  }

  override disconnectedCallback(): void {
    clearTimeout(this.pendingTimer);
    super.disconnectedCallback();
  }

  private releaseControls(): void {
    clearTimeout(this.pendingTimer);
    this.controlPending = false;
  }

  private onControl(run: RunView, control: RunControl): void {
    // aria-disabled keeps a deactivated control reachable and announced, so the click is what is
    // refused here, not the focus.
    if (!control.enabled || this.controlPending) return;
    this.controlPending = true;
    clearTimeout(this.pendingTimer);
    this.pendingTimer = setTimeout(() => this.releaseControls(), 5_000);
    if (control.action === 'fix-skill' && run.skill)
      this.send({ type: 'view-skill', skill: run.skill });
    else this.send({ type: 'run-control', action: control.action, runId: run.runId });
  }

  private loadMore(after: number): void {
    if (this.loadingMore) return;
    this.loadingMore = true;
    this.requestUpdate();
    this.send({ type: 'load-more-calls', after });
  }

  private send(message: RunWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<RunWebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  /** The reason a control is deactivated, as text: a tooltip reaches neither keyboard nor touch. */
  private controlHint(run: RunView) {
    const reason = (run.controls ?? []).find((c) => !c.enabled && c.disabledReason)?.disabledReason;
    return reason ? html`<p class="control-hint" id="control-hint">${reason}</p>` : nothing;
  }

  override render() {
    if (this.error)
      return html`
        <div class="status-message error" role="alert">
          ${this.error.message}
          ${
            this.error.canReconnect
              ? html`<button class="reconnect" @click=${() => this.send({ type: 'refresh' })}>
                  Reconnect
                </button>`
              : html`<span>Close this panel and open the run again.</span>`
          }
        </div>
      `;
    const run = this.view;
    if (!run) return html`<div class="status-message" role="status">Loading run…</div>`;
    return html`
      <header>
        <h1>${runHeading(run).title}</h1>
        <span class="chip status-chip ${run.tone}"
          >${statusIcon(run.status)}${run.status.replaceAll('_', ' ')}</span
        >
        ${
          (run.controls ?? []).length > 0
            ? html`<div class="controls" role="group" aria-label="Run controls">
                ${(run.controls ?? []).map(
                  (control) => html`
                    <button
                      class="run-control ${control.action === 'approve' ? 'primary' : ''}"
                      aria-disabled=${!control.enabled || this.controlPending ? 'true' : nothing}
                      title=${control.disabledReason ?? ''}
                      aria-describedby=${
                        !control.enabled && control.disabledReason ? 'control-hint' : nothing
                      }
                      @click=${() => this.onControl(run, control)}
                    >
                      ${control.label}
                    </button>
                  `,
                )}
              </div>`
            : nothing
        }
        ${this.controlHint(run)}
      </header>
      <div class="meta">
        <span>${runByline(run)}</span>
        <button
          class="copy-run"
          title=${run.runId}
          @click=${() => this.send({ type: 'copy-run-id' })}
        >
          Copy run id
        </button>
      </div>
      ${
        run.traceId
          ? html`<div class="trace">
              Trace ${run.traceId}
              <button
                class="copy-trace"
                title=${run.traceId}
                @click=${() => this.send({ type: 'copy-trace-id', traceId: run.traceId! })}
              >
                Copy trace id
              </button>
            </div>`
          : nothing
      }
      ${
        run.targetPageId
          ? html`<div>
              Target
              <button
                class="link"
                title="Open the target page"
                @click=${() => this.send({ type: 'open-page', pageId: run.targetPageId! })}
              >
                ${run.targetPageId}
              </button>
            </div>`
          : nothing
      }

      <section aria-label="Attempts">
        <h2>Attempts</h2>
        ${
          run.attempts.length
            ? run.attempts.map(
                (attempt) => html`
                  <div class="attempt">
                    <div class="attempt-line">
                      <strong>#${attempt.n}</strong
                      ><span
                        >${formatDateTime(attempt.startedAt) || '—'} →
                        ${formatDateTime(attempt.endedAt) || '—'}</span
                      ><span>· ${formatDuration(attempt.startedAt, attempt.endedAt) || '—'}</span
                      ><span>· ${attempt.outcome}</span>
                    </div>
                    ${attempt.error ? html`<div class="attempt-error">${attempt.error}</div>` : nothing}
                  </div>
                `,
              )
            : html`<p class="muted">${emptyAttempts(run.status)}</p>`
        }
      </section>
      <section aria-label="Plan">
        <h2>Plan</h2>
        ${
          run.plan.length
            ? run.plan.map((step) => {
                // A finished run is not still doing a step.
                const shown = displayStepStatus(step.status, run.status);
                return html`
                  <div class="plan-step ${shown}">
                    <span class="glyph" aria-hidden="true">${glyphs[shown]}</span
                    ><span>${step.step}</span
                    ><span class="step-status"
                      >${shown === 'unfinished' ? 'not finished' : shown.replace('_', ' ')}</span
                    >
                  </div>
                `;
              })
            : html`<p class="muted">${emptyPlan(run.status)}</p>`
        }
      </section>
      <section aria-label="Tool calls">
        <h2>Tool calls</h2>
        ${
          run.calls.length
            ? run.calls.map(
                (call) => html`
                  <div class="tool-call">
                    <span>${call.seq} · ${call.tool} ·</span
                    ><span
                      class=${call.status === 'error' || call.status === 'rejected' ? 'call-error' : ''}
                      >${call.status}${call.errorCode ? html` · ${call.errorCode}` : nothing}</span
                    ><span
                      >· ${call.durationMs.toFixed(1)} ms · ${call.bytes.request} request bytes /
                      ${call.bytes.response} response bytes</span
                    >
                  </div>
                `,
              )
            : run.toolCallCount && run.toolCallCount > 0
              ? html`<p class="calls-unavailable">
                  ${run.toolCallCount} tool calls reported; per-call detail is not available for
                  this run
                </p>`
              : html`<p class="muted">No tool calls reported.</p>`
        }
        ${typeof run.nextAfter === 'number' ? html`<button class="load-more" ?disabled=${this.loadingMore} @click=${() => this.loadMore(run.nextAfter!)}>Load more</button>` : nothing}
      </section>
      ${
        run.summary
          ? html`<section>
              <h2>Summary</h2>
              <div class="summary">${run.summary}</div>
            </section>`
          : nothing
      }
    `;
  }
}
customElements.define('escurel-run-detail', EscurelRunDetail);
