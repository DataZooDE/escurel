import { LitElement, css, html, nothing, type TemplateResult } from 'lit';
import type { ChatRow, ChatViewState, ChatWebviewToHost } from '../../src/shared/protocol';
import { markdownStyles, renderMarkdown } from '../shared/markdown-view';
import { theme } from '../shared/theme.css';

/**
 * The Chat view (SPEC §3.11): Recent conversations, New chat, the conversation and a prompt. It
 * renders what the host sends (cleaned rows, opaque keys) and posts the person's actions back; it
 * holds no token and resolves nothing itself.
 */
export class EscurelChat extends LitElement {
  static override properties = {
    state: { attribute: false },
  };

  declare state: ChatViewState | undefined;

  static override styles = [
    theme,
    markdownStyles,
    css`
      :host {
        display: flex;
        flex-direction: column;
        height: 100vh;
        box-sizing: border-box;
        background: var(--vscode-sideBar-background, var(--vscode-editor-background));
      }
      header {
        display: flex;
        gap: 6px;
        align-items: center;
        padding: 6px 8px;
        border-bottom: 1px solid var(--escurel-border);
      }
      header .title {
        flex: 1;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 600;
      }
      details.recent {
        padding: 2px 8px 6px;
        border-bottom: 1px solid var(--escurel-border);
      }
      details.recent ul {
        list-style: none;
        margin: 4px 0 0;
        padding: 0;
        max-height: 30vh;
        overflow: auto;
      }
      details.recent button {
        width: 100%;
        text-align: left;
        border-color: transparent;
      }
      .log {
        flex: 1;
        overflow: auto;
        padding: 8px;
        display: flex;
        flex-direction: column;
        gap: 10px;
      }
      .msg .who {
        font-size: 0.9em;
        color: var(--escurel-muted);
        margin-bottom: 2px;
      }
      .msg.user .text {
        white-space: pre-wrap;
        padding: 4px 8px;
        border-left: 2px solid var(--escurel-instance);
      }
      .msg.assistant .text {
        padding: 0 8px;
        border-left: 2px solid var(--escurel-run);
      }
      .msg.assistant.failed .text,
      .msg.assistant.cancelled .text {
        border-left-color: var(--escurel-run-failed);
      }
      .state-word {
        font-weight: 600;
      }
      .pending,
      .notice,
      .empty {
        color: var(--escurel-muted);
        font-style: italic;
      }
      .row-actions {
        margin-top: 4px;
      }
      .row-actions button {
        color: var(--vscode-textLink-foreground);
        border-color: transparent;
        padding: 0 4px;
      }
      form {
        display: flex;
        gap: 6px;
        padding: 6px 8px 8px;
        border-top: 1px solid var(--escurel-border);
      }
      textarea {
        flex: 1;
        resize: vertical;
        min-height: 2.6em;
        font: inherit;
        color: var(--vscode-input-foreground);
        background: var(--vscode-input-background);
        border: 1px solid var(--vscode-input-border, var(--escurel-border));
        padding: 4px;
      }
      textarea:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
      }
      .send {
        align-self: flex-end;
        color: var(--vscode-button-foreground);
        background: var(--vscode-button-background);
      }
      .status {
        padding: 12px 8px;
        color: var(--escurel-muted);
      }
    `,
  ];

  private emit(message: ChatWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<ChatWebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  private readonly onWikilink = (e: Event): void => {
    e.stopPropagation();
    this.emit({ type: 'open-wikilink', wikilink: (e as CustomEvent<string>).detail });
  };

  override connectedCallback(): void {
    super.connectedCallback();
    this.addEventListener('escurel-wikilink', this.onWikilink);
  }

  override disconnectedCallback(): void {
    this.removeEventListener('escurel-wikilink', this.onWikilink);
    super.disconnectedCallback();
  }

  private send(form: HTMLFormElement): void {
    const box = form.querySelector('textarea')!;
    const text = box.value.trim();
    if (!text || !this.state?.canSend) return;
    this.emit({ type: 'send', text });
    box.value = '';
  }

  private onKeydown(e: KeyboardEvent): void {
    // Enter sends, Shift+Enter is a new line (NFR-U).
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    this.send((e.currentTarget as HTMLTextAreaElement).form!);
  }

  private row(r: ChatRow): TemplateResult {
    switch (r.kind) {
      case 'user':
        return html`<div class="msg user" role="article" aria-label="Your question">
          <div class="who">You</div>
          <div class="text">${r.text}</div>
        </div>`;
      case 'assistant': {
        const word = r.status === 'failed' ? 'Failed' : r.status === 'cancelled' ? 'Stopped' : '';
        return html`<div
          class="msg assistant ${r.status}"
          role="article"
          aria-label=${word ? `Answer (${word.toLowerCase()})` : 'Answer'}
        >
          <div class="who">
            Escurel${word ? html` · <span class="state-word">${word}</span>` : nothing}
          </div>
          <div class="text">${renderMarkdown(r.text)}</div>
          ${
            r.runId
              ? html`<div class="row-actions">
                  <button
                    type="button"
                    class="show-run"
                    @click=${() => this.emit({ type: 'show-run', key: r.key })}
                  >
                    Show how I got this
                  </button>
                </div>`
              : nothing
          }
        </div>`;
      }
      case 'pending':
        return html`<div class="pending" role="status" aria-busy="true">${r.text}</div>`;
      case 'notice':
        return html`<div class="notice" role="status">${r.text}</div>`;
    }
  }

  override render(): TemplateResult {
    const s = this.state;
    if (!s) return html`<div class="status" role="status">Loading…</div>`;
    if (s.status !== 'ready') {
      return html`<div class="status" role="status">${s.message ?? ''}</div>`;
    }
    return html`
      <header>
        <span class="title">${s.current?.title ?? 'New chat'}</span>
        <button type="button" class="new" @click=${() => this.emit({ type: 'new-chat' })}>
          New chat
        </button>
      </header>
      ${
        s.recent.length
          ? html`<details class="recent">
              <summary>Recent</summary>
              <ul>
                ${s.recent.map(
                  (c) =>
                    html`<li>
                      <button
                        type="button"
                        aria-current=${s.current?.key === c.key ? 'true' : 'false'}
                        @click=${() => this.emit({ type: 'open-conversation', key: c.key })}
                      >
                        ${c.title}
                      </button>
                    </li>`,
                )}
              </ul>
            </details>`
          : nothing
      }
      <div class="log" role="log" aria-live="polite" aria-label="Conversation">
        ${
          s.rows.length
            ? s.rows.map((r) => this.row(r))
            : html`<p class="empty">Ask a question about what this knowledge base knows.</p>`
        }
      </div>
      <form
        @submit=${(e: Event) => {
          e.preventDefault();
          this.send(e.currentTarget as HTMLFormElement);
        }}
      >
        <textarea
          aria-label="Ask a question"
          placeholder="Ask a question"
          title="Enter sends, Shift+Enter starts a new line"
          @keydown=${(e: KeyboardEvent) => this.onKeydown(e)}
        ></textarea>
        <button type="submit" class="send" ?disabled=${!s.canSend}>Send</button>
      </form>
    `;
  }
}

customElements.define('escurel-chat', EscurelChat);
