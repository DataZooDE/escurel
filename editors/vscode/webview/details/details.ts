import { LitElement, css, html } from 'lit';
import type {
  DetailsAction,
  DetailsWebviewToHost,
  ShownDetails,
  ThreadWebviewToHost,
} from '../../src/shared/protocol';
import { theme } from '../shared/theme.css';
import '../thread/inspector';

/**
 * The details of the node selected in a thread, as a view of its own in VS Code's panel area (the
 * user docks, moves and resizes it with VS Code's own layout). It wraps the inspector component
 * and adds the two things a view of its own needs: an empty state, and the thread's id on every
 * message, because the host acts only for the thread whose node it is showing.
 */
export class EscurelDetails extends LitElement {
  static override properties = {
    shown: { attribute: false },
  };

  declare shown: ShownDetails | undefined;

  static override styles = [
    theme,
    css`
      :host {
        display: block;
        height: 100%;
        overflow: auto;
        box-sizing: border-box;
      }
      .empty {
        margin: 0;
        padding: 16px;
        color: var(--escurel-muted);
      }
    `,
  ];

  override connectedCallback(): void {
    super.connectedCallback();
    this.addEventListener('keydown', this.onKeydown);
  }

  override disconnectedCallback(): void {
    this.removeEventListener('keydown', this.onKeydown);
    super.disconnectedCallback();
  }

  private readonly onKeydown = (e: KeyboardEvent): void => {
    // An open Skill menu uses Escape to close itself; only a bare Escape leaves the view.
    if (e.key !== 'Escape' || e.defaultPrevented || !this.shown) return;
    this.emit({ type: 'focus-canvas', rootEventId: this.shown.rootEventId });
  };

  private emit(message: DetailsWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<DetailsWebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  private onInspectorMessage(e: CustomEvent<ThreadWebviewToHost>): void {
    // The inspector speaks the thread's protocol; this view re-addresses it to its thread.
    e.stopPropagation();
    const shown = this.shown;
    const m = e.detail;
    if (!shown) return;
    if (m.type !== 'start-skill' && m.type !== 'view-skill' && m.type !== 'run-control') return;
    this.emit({
      type: 'details-action',
      rootEventId: shown.rootEventId,
      message: m as DetailsAction,
    });
  }

  override render() {
    const shown = this.shown;
    if (!shown) {
      return html`<p class="empty">Select a node in a thread to see its details.</p>`;
    }
    return html`<escurel-thread-inspector
      .nodeId=${shown.nodeId}
      .detail=${shown.detail}
      @escurel-message=${this.onInspectorMessage}
    ></escurel-thread-inspector>`;
  }
}

if (!customElements.get('escurel-details')) {
  customElements.define('escurel-details', EscurelDetails as unknown as CustomElementConstructor);
}
