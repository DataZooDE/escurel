import { LitElement, css, html, nothing } from 'lit';
import type { PropertyValues } from 'lit';
import { property, state } from 'lit/decorators.js';
import type { TranscriptKind, TranscriptRecord, TranscriptState } from '../../src/shared/protocol';
import { formatElapsed } from '../../src/shared/time';
import { theme } from '../shared/theme.css';

/** Each kind has a glyph AND a word: a kind is never told by its colour or shape alone. */
const KINDS: { kind: TranscriptKind; glyph: string; word: string; plural: string }[] = [
  { kind: 'tool_call', glyph: '→', word: 'Tool call', plural: 'Tool calls' },
  { kind: 'tool_result', glyph: '←', word: 'Result', plural: 'Results' },
  { kind: 'message', glyph: '¶', word: 'Message', plural: 'Messages' },
  { kind: 'error', glyph: '!', word: 'Error', plural: 'Errors' },
  { kind: 'command', glyph: '$', word: 'Command', plural: 'Commands' },
];
const META = new Map(KINDS.map((k) => [k.kind, k]));

/**
 * The run transcript as a replay: every record the agent's harness wrote, in time order,
 * filterable by kind, and steppable one record at a time. It holds no data of its own: the host reads
 * the transcript and hands over cleaned rows.
 */
export class EscurelRunTranscript extends LitElement {
  static styles = [
    theme,
    css`
      :host {
        display: block;
      }
      .toolbar {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 6px;
        margin: 0 0 8px;
      }
      .kinds {
        display: flex;
        flex-wrap: wrap;
        gap: 4px;
      }
      button {
        font: inherit;
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        padding: 2px 8px;
        cursor: pointer;
      }
      button:hover:not([aria-disabled='true']) {
        background: var(--vscode-button-secondaryHoverBackground);
      }
      button:focus-visible,
      .records:focus-visible {
        outline: 2px solid var(--vscode-focusBorder);
        outline-offset: 2px;
      }
      button[aria-pressed='false'] {
        background: transparent;
        color: var(--escurel-muted);
        text-decoration: line-through;
      }
      button[aria-disabled='true'] {
        opacity: 0.55;
        cursor: not-allowed;
      }
      .stepper {
        display: flex;
        align-items: center;
        gap: 6px;
        margin-left: auto;
      }
      .step-status {
        color: var(--escurel-muted);
      }
      .records {
        list-style: none;
        margin: 0;
        padding: 0;
      }
      .record {
        display: grid;
        grid-template-columns: 6.5em 7em 1fr auto;
        gap: 2px 10px;
        align-items: baseline;
        padding: 5px 4px;
        border-bottom: 1px solid var(--escurel-border);
      }
      .record[aria-current='step'] {
        background: var(--vscode-list-inactiveSelectionBackground);
        outline: 1px solid var(--vscode-focusBorder);
      }
      .kind {
        font-weight: 600;
        white-space: nowrap;
      }
      .glyph {
        display: inline-block;
        width: 1.1em;
        margin-right: 4px;
        text-align: center;
      }
      .record.error .kind,
      .record.error .summary {
        color: var(--vscode-errorForeground);
      }
      .name {
        font-family: var(--vscode-editor-font-family, monospace);
        overflow-wrap: anywhere;
      }
      .summary {
        grid-column: 3;
        overflow-wrap: anywhere;
      }
      .when {
        grid-column: 4;
        grid-row: 1;
        white-space: nowrap;
        color: var(--escurel-muted);
        font-size: 0.9em;
      }
      .offset {
        grid-column: 1;
        grid-row: 1;
        color: var(--escurel-muted);
        font-variant-numeric: tabular-nums;
      }
      .record .kind {
        grid-column: 2;
        grid-row: 1;
      }
      .record .name {
        grid-column: 3;
        grid-row: 1;
      }
      .record .summary {
        grid-row: 2;
      }
      .note,
      .transcript-empty {
        color: var(--escurel-muted);
      }
      .transcript-empty.failed {
        color: var(--vscode-errorForeground);
      }
    `,
  ];

  @property({ attribute: false }) state?: TranscriptState;
  @state() private hiddenKinds = new Set<TranscriptKind>();
  /** The record the replay stands on, or `undefined` while every record is shown. */
  @state() private cursor: number | undefined;

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has('state') && this.state?.status !== 'ready') this.cursor = undefined;
  }

  private shown(records: TranscriptRecord[]): TranscriptRecord[] {
    return records.filter((r) => !this.hiddenKinds.has(r.kind));
  }

  private toggle(kind: TranscriptKind): void {
    const next = new Set(this.hiddenKinds);
    if (next.has(kind)) next.delete(kind);
    else next.add(kind);
    this.hiddenKinds = next;
    this.cursor = undefined;
  }

  private step(to: number, count: number): void {
    if (count === 0) return;
    this.cursor = Math.min(Math.max(0, to), count - 1);
  }

  /** Start the replay at the first record; the button pressed is replaced, so the focus moves to Next. */
  private async begin(count: number): Promise<void> {
    this.step(0, count);
    await this.updateComplete;
    this.renderRoot.querySelector<HTMLButtonElement>('.step-next')?.focus();
  }

  private onKey(event: KeyboardEvent, count: number): void {
    if (this.cursor === undefined) return;
    const moves: Record<string, number> = {
      ArrowRight: this.cursor + 1,
      ArrowDown: this.cursor + 1,
      ArrowLeft: this.cursor - 1,
      ArrowUp: this.cursor - 1,
      Home: 0,
      End: count - 1,
    };
    const to = moves[event.key];
    if (to === undefined) return;
    event.preventDefault();
    this.step(to, count);
  }

  override render() {
    const s = this.state;
    if (!s || s.status === 'loading')
      return html`<p class="note" role="status">Loading the transcript…</p>`;
    if (s.status !== 'ready')
      return html`<p
        class="transcript-empty ${s.status === 'error' ? 'failed' : ''}"
        role=${s.status === 'error' ? 'alert' : 'status'}
      >
        ${s.message}
      </p>`;
    if (s.records.length === 0)
      return html`<p class="transcript-empty" role="status">The transcript is empty.</p>`;
    const all = this.shown(s.records);
    const stepping = this.cursor !== undefined;
    const visible = stepping ? all.slice(0, this.cursor! + 1) : all;
    const current = stepping ? all[this.cursor!] : undefined;
    const attempts = new Set(s.records.map((r) => r.attempt)).size;
    return html`
      <div class="toolbar">
        <div class="kinds" role="group" aria-label="Show kinds">
          ${KINDS.map(({ kind, plural }) => {
            const count = s.records.filter((r) => r.kind === kind).length;
            return count
              ? html`<button
                  class="kind-filter"
                  aria-pressed=${this.hiddenKinds.has(kind) ? 'false' : 'true'}
                  @click=${() => this.toggle(kind)}
                >
                  ${plural} (${count})
                </button>`
              : nothing;
          })}
        </div>
        <div class="stepper" role="group" aria-label="Replay">
          ${
            stepping
              ? html`
                  <button
                    aria-disabled=${this.cursor === 0 ? 'true' : nothing}
                    @click=${() => this.step(this.cursor! - 1, all.length)}
                  >
                    Previous
                  </button>
                  <button
                    class="step-next"
                    aria-disabled=${this.cursor === all.length - 1 ? 'true' : nothing}
                    @click=${() => this.step(this.cursor! + 1, all.length)}
                  >
                    Next
                  </button>
                  <button @click=${() => (this.cursor = undefined)}>Show all</button>
                  <span class="step-status" role="status" aria-live="polite"
                    >Step ${this.cursor! + 1} of ${all.length}:
                    ${META.get(current!.kind)!.word}</span
                  >
                `
              : html`<button
                  aria-disabled=${all.length === 0 ? 'true' : nothing}
                  @click=${() => void this.begin(all.length)}
                >
                  Step through
                </button>`
          }
        </div>
      </div>
      ${
        all.length === 0
          ? html`<p class="transcript-empty">Every kind is hidden. Turn one back on above.</p>`
          : html`<ol
              class="records"
              aria-label="Transcript"
              tabindex=${stepping ? '0' : nothing}
              @keydown=${(e: KeyboardEvent) => this.onKey(e, all.length)}
            >
              ${visible.map((r) => {
                const meta = META.get(r.kind)!;
                return html`<li
                  class="record ${r.kind === 'error' ? 'error' : ''}"
                  data-kind=${r.kind}
                  aria-current=${current && current.i === r.i ? 'step' : nothing}
                >
                  <span class="offset"
                    >${r.offsetMs !== undefined ? `+${formatElapsed(r.offsetMs, { tenths: true })}` : ''}</span
                  >
                  <span class="kind"
                    ><span class="glyph" aria-hidden="true">${meta.glyph}</span>${meta.word}</span
                  >
                  ${r.name ? html`<span class="name" title=${r.digest ?? ''}>${r.name}</span>` : nothing}
                  ${attempts > 1 || r.attempt > 1 ? html`<span class="when">attempt ${r.attempt}</span>` : nothing}
                  <span class="summary">${r.summary}</span>
                </li>`;
              })}
            </ol>`
      }
    `;
  }
}
customElements.define('escurel-run-transcript', EscurelRunTranscript);
