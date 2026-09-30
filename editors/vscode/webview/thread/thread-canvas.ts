import { LitElement, css, html, nothing, svg } from 'lit';
import type { PropertyValues } from 'lit';
import { property, state } from 'lit/decorators.js';
import type {
  FocusGraph,
  InspectorView,
  LaidOutNode,
  ThreadLayout,
  ThreadNode,
  ThreadView,
  ThreadWebviewToHost,
} from '../../src/shared/protocol';
import { theme } from '../shared/theme.css';
import './inspector';
import { fitToBounds, panToReveal, zoomAboutPoint } from './viewport';
import type { ViewportState } from './viewport';

export class EscurelThreadCanvas extends LitElement {
  static override styles = [
    theme,
    css`
      :host {
        display: flex;
        flex-direction: column;
        width: 100%;
        height: 100%;
        overflow: hidden;
        position: relative;
        background: var(--vscode-editor-background);
      }
      .toolbar {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 6px 12px;
        background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
        border-bottom: 1px solid var(--escurel-border);
        flex-shrink: 0;
        z-index: 10;
      }
      .toolbar button {
        background: var(--vscode-button-secondaryBackground);
        color: var(--vscode-button-secondaryForeground, var(--vscode-foreground));
        border: 1px solid var(--vscode-button-border, transparent);
        border-radius: 2px;
        padding: 3px 8px;
        font-size: 0.85em;
        cursor: pointer;
      }
      .toolbar button:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      .toolbar button:hover {
        background: var(
          --vscode-button-secondaryHoverBackground,
          var(--vscode-button-hoverBackground)
        );
      }
      .zoom-level {
        font-size: 0.85em;
        color: var(--escurel-muted);
        min-width: 44px;
        text-align: center;
      }
      .main-split {
        display: flex;
        flex: 1;
        position: relative;
        overflow: hidden;
      }
      .canvas-area {
        flex: 1;
        position: relative;
        overflow: hidden;
        cursor: grab;
      }
      .canvas-area.panning {
        cursor: grabbing;
      }
      .header-pinned-strip {
        position: absolute;
        top: 0;
        left: 0;
        right: 0;
        height: 28px;
        overflow: hidden;
        pointer-events: none;
        z-index: 5;
      }
      .header-transformed-track {
        position: absolute;
        top: 0;
        left: 0;
        height: 100%;
        transform-origin: 0 0;
      }
      .column-header {
        position: absolute;
        top: 6px;
        font-size: 0.8em;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--escurel-muted);
        white-space: nowrap;
        pointer-events: none;
      }
      .transformed-viewport {
        position: absolute;
        top: 0;
        left: 0;
        transform-origin: 0 0;
      }
      .wires-layer {
        position: absolute;
        top: 0;
        left: 0;
        pointer-events: none;
        overflow: visible;
      }
      .wire {
        fill: none;
        stroke: var(--escurel-border);
        stroke-width: 1.5px;
      }
      .wire.solid {
        stroke-dasharray: none;
      }
      .wire.promoted {
        stroke-dasharray: 6 3;
      }
      .wire.sent {
        stroke-dasharray: 2 3;
      }
      .wire.emphasised {
        stroke: var(--vscode-focusBorder);
        stroke-width: 3px;
      }
      .card {
        position: absolute;
        box-sizing: border-box;
        background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
        border: 1px solid var(--escurel-border);
        border-radius: 4px;
        padding: 6px 10px;
        overflow: hidden;
        display: flex;
        flex-direction: column;
        justify-content: space-between;
        cursor: pointer;
        outline: none;
      }
      .card:hover {
        border-color: var(--vscode-focusBorder);
      }
      .card.selected {
        border-color: var(--vscode-focusBorder);
        box-shadow: 0 0 0 1px var(--vscode-focusBorder);
      }
      .card:focus-visible {
        outline: 2px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      .card-header {
        display: flex;
        align-items: center;
        gap: 6px;
        min-width: 0;
      }
      .tone-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        flex-shrink: 0;
      }
      .tone-dot.event {
        background: var(--escurel-event);
      }
      .tone-dot.instance {
        background: var(--escurel-instance);
      }
      .tone-dot.run {
        background: var(--escurel-run);
      }
      .tone-dot.failed {
        background: var(--escurel-run-failed);
      }
      .tone-dot.skill {
        background: var(--escurel-skill);
      }
      .tone-dot.neutral {
        background: var(--escurel-muted);
      }
      .card-title {
        font-weight: 600;
        font-size: 0.9em;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        flex: 1;
      }
      .card-subtitle {
        font-size: 0.8em;
        color: var(--escurel-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .collapse-toggle {
        font-size: 0.75em;
        padding: 0 4px;
        line-height: 1.2;
        border: 1px solid var(--escurel-border);
        border-radius: 2px;
        color: var(--escurel-muted);
        background: transparent;
      }
      .collapse-toggle:hover {
        color: var(--vscode-foreground);
        border-color: var(--vscode-focusBorder);
      }
      .meta-lines {
        display: flex;
        flex-direction: column;
        gap: 2px;
        margin: 2px 0;
      }
      .meta-line {
        font-size: 0.75em;
        color: var(--escurel-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .card-footer {
        display: flex;
        align-items: center;
        gap: 6px;
        flex-wrap: wrap;
        margin-top: 2px;
      }
      .gate-actions {
        display: inline-flex;
        gap: 4px;
      }
      .promote-btn {
        background: var(--escurel-run);
        color: var(--vscode-button-foreground);
        border: 1px solid transparent;
        font-size: 0.75em;
        padding: 1px 6px;
        border-radius: 2px;
      }
      .discard-btn {
        background: var(--vscode-button-secondaryBackground);
        color: var(--vscode-errorForeground);
        border: 1px solid var(--vscode-button-border, transparent);
        font-size: 0.75em;
        padding: 1px 6px;
        border-radius: 2px;
      }
      .status-message {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 12px;
        padding: 48px 16px;
        color: var(--escurel-muted);
        font-size: 1.1em;
      }
      .status-message.error {
        color: var(--vscode-errorForeground);
      }
      .reconnect {
        background: var(--vscode-button-background);
        color: var(--vscode-button-foreground);
        border: 1px solid transparent;
        padding: 4px 12px;
      }
      .reconnect:hover {
        background: var(--vscode-button-hoverBackground);
      }
      .inspector-container {
        width: 320px;
        border-left: 1px solid var(--escurel-border);
        overflow-y: auto;
        background: var(--vscode-sideBar-background, var(--vscode-editor-background));
        flex-shrink: 0;
        padding: 12px;
        box-sizing: border-box;
      }
    `,
  ];

  @property({ attribute: false }) view?: ThreadView;
  @property({ attribute: false }) layout?: ThreadLayout;
  // @ts-expect-error Specification requires property named 'focus' for FocusGraph
  @property({ attribute: false }) override focus?: FocusGraph;
  @property({ attribute: false }) details?: Record<string, InspectorView>;
  @property({ attribute: false }) error?: { message: string; canReconnect: boolean };

  @state() focusedNodeId = '';
  @state() selectedNodeId = '';
  @state() hoveredNodeId = '';
  @state() viewport: ViewportState = { x: 0, y: 0, zoom: 1.0 };
  @state() private isPanning = false;

  private panStart = { x: 0, y: 0 };
  private viewportStart = { x: 0, y: 0 };

  protected override willUpdate(changed: PropertyValues<this>): void {
    // A selection names a node of the thread on screen. When the thread is replaced or a node
    // disappears between loads, keeping it opened an inspector for an id the new thread does
    // not contain, with nothing to show.
    if (
      this.selectedNodeId &&
      (changed.has('view') || changed.has('layout')) &&
      !this.view?.nodes.some((n) => n.id === this.selectedNodeId)
    ) {
      this.selectedNodeId = '';
    }
    if (changed.has('focus') || changed.has('layout')) {
      const visibleNodes = this.layout?.nodes.filter((n) => !n.hidden) ?? [];
      const isCurrentValid = visibleNodes.some((n) => n.id === this.focusedNodeId);
      if (!isCurrentValid) {
        this.focusedNodeId =
          this.focus?.first && visibleNodes.some((n) => n.id === this.focus?.first)
            ? this.focus.first
            : (visibleNodes[0]?.id ?? '');
      }
    }
  }

  public selectNode(nodeId: string): void {
    this.selectedNodeId = nodeId;
    this.focusedNodeId = nodeId;
    this.panToFocusedNode();
    // Selecting opens the inspector, which takes its share of the width from the canvas. The
    // reveal above used the canvas as it was BEFORE that, so a card near the right edge could
    // be clipped the moment it was selected; reveal again once the layout has settled.
    void this.updateComplete.then(() => this.panToFocusedNode());
  }

  public focusNode(nodeId: string): void {
    this.focusedNodeId = nodeId;
    this.panToFocusedNode();
    this.updateComplete.then(() => {
      const cardEl = this.shadowRoot?.querySelector<HTMLElement>(`.card[data-node-id="${nodeId}"]`);
      cardEl?.focus();
    });
  }

  public panToFocusedNode(): void {
    const nodeLayout = this.layout?.nodes.find((n) => n.id === this.focusedNodeId);
    const containerEl = this.shadowRoot?.querySelector('.canvas-area');
    if (nodeLayout && containerEl) {
      const containerSize = {
        width: containerEl.clientWidth || 800,
        height: containerEl.clientHeight || 600,
      };
      this.viewport = panToReveal(this.viewport, nodeLayout, containerSize, 20);
    }
  }

  public fit(): void {
    if (!this.layout) return;
    const containerEl = this.shadowRoot?.querySelector('.canvas-area');
    const containerSize = {
      width: containerEl?.clientWidth || 800,
      height: containerEl?.clientHeight || 600,
    };
    this.viewport = fitToBounds(this.layout.bounds, containerSize, 20);
  }

  public zoomBy(factor: number): void {
    const containerEl = this.shadowRoot?.querySelector('.canvas-area');
    const center = {
      x: (containerEl?.clientWidth || 800) / 2,
      y: (containerEl?.clientHeight || 600) / 2,
    };
    this.viewport = zoomAboutPoint(this.viewport, center, this.viewport.zoom * factor);
  }

  private send(message: ThreadWebviewToHost): void {
    this.dispatchEvent(
      new CustomEvent<ThreadWebviewToHost>('escurel-message', {
        detail: message,
        bubbles: true,
        composed: true,
      }),
    );
  }

  private handlePointerDown(e: PointerEvent): void {
    // Only drag when clicking the canvas background itself.
    const target = e.target as HTMLElement;
    if (target.closest('.card') || target.closest('.toolbar') || target.closest('button')) {
      return;
    }
    this.isPanning = true;
    this.panStart = { x: e.clientX, y: e.clientY };
    this.viewportStart = { x: this.viewport.x, y: this.viewport.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  private handlePointerMove(e: PointerEvent): void {
    if (!this.isPanning) return;
    const dx = e.clientX - this.panStart.x;
    const dy = e.clientY - this.panStart.y;
    this.viewport = {
      ...this.viewport,
      x: this.viewportStart.x + dx,
      y: this.viewportStart.y + dy,
    };
  }

  private handlePointerUp(e: PointerEvent): void {
    if (!this.isPanning) return;
    this.isPanning = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      // Pointer capture might have already been released.
    }
  }

  private handleWheel(e: WheelEvent): void {
    e.preventDefault();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const cursor = { x: e.clientX - rect.left, y: e.clientY - rect.top };
    const factor = e.deltaY < 0 ? 1.1 : 0.9;
    this.viewport = zoomAboutPoint(this.viewport, cursor, this.viewport.zoom * factor);
  }

  private handleCardKeydown(e: KeyboardEvent, nodeId: string): void {
    // Only keys pressed ON the card are the card's. A keydown that bubbled up from a button
    // inside it (Promote, Discard, collapse) belongs to that button: acting on it here opened
    // the card and cancelled the button's own activation, so none of them worked from the
    // keyboard.
    if (e.target !== e.currentTarget) return;
    switch (e.key) {
      case 'ArrowRight': {
        const nextId = this.focus?.steps[nodeId]?.next;
        if (nextId) {
          e.preventDefault();
          this.focusNode(nextId);
        }
        break;
      }
      case 'ArrowLeft': {
        const backId = this.focus?.steps[nodeId]?.back;
        if (backId) {
          e.preventDefault();
          this.focusNode(backId);
        }
        break;
      }
      case 'ArrowUp': {
        const upId = this.focus?.steps[nodeId]?.up;
        if (upId) {
          e.preventDefault();
          this.focusNode(upId);
        }
        break;
      }
      case 'ArrowDown': {
        const downId = this.focus?.steps[nodeId]?.down;
        if (downId) {
          e.preventDefault();
          this.focusNode(downId);
        }
        break;
      }
      case 'Enter': {
        e.preventDefault();
        this.send({ type: 'open-node', nodeId });
        break;
      }
      case ' ': {
        e.preventDefault();
        this.selectNode(nodeId);
        this.send({ type: 'select-node', nodeId });
        break;
      }
      case 'Escape': {
        e.preventDefault();
        const toolbarFirstBtn =
          this.shadowRoot?.querySelector<HTMLButtonElement>('.toolbar button');
        toolbarFirstBtn?.focus();
        break;
      }
      case '+':
      case '=': {
        e.preventDefault();
        this.zoomBy(1.25);
        break;
      }
      case '-':
      case '_': {
        e.preventDefault();
        this.zoomBy(0.8);
        break;
      }
      case '0': {
        e.preventDefault();
        this.fit();
        break;
      }
    }
  }

  private renderCard(node: ThreadNode, layoutNode: LaidOutNode) {
    const isFocused = node.id === this.focusedNodeId;
    const isSelected = node.id === this.selectedNodeId;
    const accessibleName = [node.title, node.subtitle, node.state].filter(Boolean).join(', ');

    // Check if any child is hidden by collapse to derive expansion state.
    const isCollapsed = node.children.some((childId) => {
      const childLayout = this.layout?.nodes.find((n) => n.id === childId);
      return childLayout?.hidden === true;
    });

    return html`
      <div
        class="card ${isSelected ? 'selected' : ''}"
        data-node-id="${node.id}"
        role="treeitem"
        aria-level="${layoutNode.column + 1}"
        aria-label="${accessibleName}"
        aria-expanded="${node.collapsible ? (isCollapsed ? 'false' : 'true') : nothing}"
        tabindex="${isFocused ? '0' : '-1'}"
        style="left: ${layoutNode.x}px; top: ${layoutNode.y}px; width: ${layoutNode.width}px; height: ${layoutNode.height}px;"
        @click=${() => {
          this.selectNode(node.id);
          this.send({ type: 'select-node', nodeId: node.id });
        }}
        @dblclick=${() => this.send({ type: 'open-node', nodeId: node.id })}
        @pointerenter=${() => (this.hoveredNodeId = node.id)}
        @pointerleave=${() => {
          if (this.hoveredNodeId === node.id) this.hoveredNodeId = '';
        }}
        @keydown=${(e: KeyboardEvent) => this.handleCardKeydown(e, node.id)}
      >
        <div class="card-header">
          <span class="tone-dot ${node.tone}"></span>
          <span class="card-title" title="${node.title}">${node.title}</span>
          ${
            node.collapsible
              ? html`<button
                  class="collapse-toggle"
                  aria-label="Toggle collapse ${node.title}"
                  @click=${(e: Event) => {
                    e.stopPropagation();
                    this.send({ type: 'toggle-collapse', nodeId: node.id });
                  }}
                >
                  ${isCollapsed ? '▶' : '▼'}
                </button>`
              : nothing
          }
        </div>
        ${
          node.subtitle
            ? html`<div class="card-subtitle" title="${node.subtitle}">${node.subtitle}</div>`
            : nothing
        }
        ${
          node.meta.length
            ? html`<div class="meta-lines">
                ${node.meta.slice(0, 4).map((line) => html`<div class="meta-line">${line}</div>`)}
              </div>`
            : nothing
        }
        <div class="card-footer">
          ${node.chips.map((chip) => html`<span class="chip ${chip.tone}">${chip.text}</span>`)}
          ${
            node.gate
              ? html`<div class="gate-actions">
                  <button
                    class="promote-btn"
                    @click=${(e: Event) => {
                      e.stopPropagation();
                      this.send({
                        type: 'promote',
                        changesetId: node.gate?.changesetId,
                        draftId: node.gate?.draftId,
                      });
                    }}
                  >
                    ${node.kind === 'changeset' ? `Promote all ${node.gate.drafts}` : 'Promote'}
                  </button>
                  <button
                    class="discard-btn"
                    @click=${(e: Event) => {
                      e.stopPropagation();
                      this.send({
                        type: 'discard',
                        changesetId: node.gate?.changesetId,
                        draftId: node.gate?.draftId,
                      });
                    }}
                  >
                    Discard
                  </button>
                </div>`
              : nothing
          }
        </div>
      </div>
    `;
  }

  override render() {
    if (this.error) {
      return html`
        <div class="status-message error" role="alert">
          <span>${this.error.message}</span>
          ${
            this.error.canReconnect
              ? html`<button class="reconnect" @click=${() => this.send({ type: 'refresh' })}>
                  Reconnect
                </button>`
              : html`<span class="muted">Close this panel and open the thread again.</span>`
          }
        </div>
      `;
    }

    if (!this.view || !this.layout || !this.focus) {
      return html`<div class="status-message" role="status">Loading thread…</div>`;
    }

    if (this.view.nodes.length === 0) {
      return html`<div class="status-message empty" role="status">No events in this thread.</div>`;
    }

    const nodeMap = new Map<string, ThreadNode>(this.view.nodes.map((n) => [n.id, n]));
    const visibleLaidOutNodes = this.layout.nodes.filter((n) => !n.hidden);

    return html`
      <div class="toolbar" role="toolbar" aria-label="Thread canvas controls">
        <button aria-label="Zoom out" @click=${() => this.zoomBy(0.8)}>−</button>
        <span class="zoom-level" aria-live="polite">${Math.round(this.viewport.zoom * 100)}%</span>
        <button aria-label="Zoom in" @click=${() => this.zoomBy(1.25)}>+</button>
        <button aria-label="Fit graph to view" data-action="fit" @click=${() => this.fit()}>
          Fit
        </button>
        <button
          aria-label="Expand all"
          data-action="expand-all"
          @click=${() => this.send({ type: 'expand-all' })}
        >
          Expand all
        </button>
      </div>

      <div class="main-split">
        <div
          class="canvas-area ${this.isPanning ? 'panning' : ''}"
          role="tree"
          aria-label="Thread execution tree"
          @pointerdown=${this.handlePointerDown}
          @pointermove=${this.handlePointerMove}
          @pointerup=${this.handlePointerUp}
          @pointercancel=${this.handlePointerUp}
          @wheel=${this.handleWheel}
        >
          <div class="header-pinned-strip">
            <div
              class="header-transformed-track"
              style="transform: translateX(${this.viewport.x}px) scaleX(${this.viewport.zoom});"
            >
              ${this.layout.columnHeaders.map(
                (col) =>
                  html`<div class="column-header" style="left: ${col.x}px;">${col.label}</div>`,
              )}
            </div>
          </div>

          <div
            class="transformed-viewport"
            style="transform: translate(${this.viewport.x}px, ${this.viewport.y}px) scale(${this.viewport.zoom});"
          >
            <svg
              class="wires-layer"
              width="${this.layout.bounds.width}"
              height="${this.layout.bounds.height}"
            >
              ${this.layout.wires.map((wire) => {
                const isEmphasised =
                  wire.from === this.selectedNodeId ||
                  wire.to === this.selectedNodeId ||
                  wire.from === this.hoveredNodeId ||
                  wire.to === this.hoveredNodeId;
                return svg`<path
                  d="${wire.path}"
                  class="wire ${wire.style} ${isEmphasised ? 'emphasised' : ''}"
                />`;
              })}
            </svg>

            ${visibleLaidOutNodes.map((laidOut) => {
              const node = nodeMap.get(laidOut.id);
              return node ? this.renderCard(node, laidOut) : nothing;
            })}
          </div>
        </div>

        ${
          this.selectedNodeId
            ? html`<div class="inspector-container">
                <escurel-thread-inspector
                  .detail=${this.details?.[this.selectedNodeId]}
                ></escurel-thread-inspector>
              </div>`
            : nothing
        }
      </div>
    `;
  }
}

if (!customElements.get('escurel-thread-canvas')) {
  customElements.define(
    'escurel-thread-canvas',
    EscurelThreadCanvas as unknown as CustomElementConstructor,
  );
}
