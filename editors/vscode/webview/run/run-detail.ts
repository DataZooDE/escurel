import { LitElement, css, html, nothing } from 'lit';
import type { PropertyValues } from 'lit';
import { property } from 'lit/decorators.js';
import type { RunView, RunWebviewToHost } from '../../src/shared/protocol';
import { formatDateTime, formatDuration } from '../../src/shared/time';
import { theme } from '../shared/theme.css';

const glyphs = { completed: '✓', in_progress: '◐', pending: '○', blocked: '!' };

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
      .trace {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
      }
      .meta {
        color: var(--escurel-muted);
      }
      .status-chip {
        border: 1px solid currentColor;
        color: var(--escurel-run);
      }
      .status-chip.failed {
        color: var(--escurel-run-failed);
      }
      .status-chip.neutral {
        color: var(--escurel-muted);
      }
      .link,
      .copy-trace {
        color: var(--vscode-textLink-foreground);
        text-decoration: underline;
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

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has('view')) this.loadingMore = false;
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
        <h1>Run ${run.runId}</h1>
        <span class="chip status-chip ${run.tone}">${run.status.replaceAll('_', ' ')}</span>
      </header>
      <div class="meta">
        ${run.harness ? html`<span>Harness ${run.harness}</span>` : nothing}
        ${run.model ? html`<span>· Model ${run.model}</span>` : nothing}
        ${run.autonomy ? html`<span>· Autonomy ${run.autonomy}</span>` : nothing}
        ${run.depth !== undefined ? html`<span>· Depth ${run.depth}</span>` : nothing}
      </div>
      ${
        run.traceId
          ? html`<div class="trace">
              Trace ${run.traceId}
              <button
                class="copy-trace"
                aria-label="Copy trace id ${run.traceId}"
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
                aria-label="Open target page ${run.targetPageId}"
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
            : html`<p class="muted">No attempts reported.</p>`
        }
      </section>
      <section aria-label="Plan">
        <h2>Plan</h2>
        ${
          run.plan.length
            ? run.plan.map(
                (step) => html`
                  <div class="plan-step ${step.status}">
                    <span class="glyph" aria-hidden="true">${glyphs[step.status]}</span
                    ><span>${step.step}</span
                    ><span class="step-status">${step.status.replace('_', ' ')}</span>
                  </div>
                `,
              )
            : html`<p class="muted">No plan reported.</p>`
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
