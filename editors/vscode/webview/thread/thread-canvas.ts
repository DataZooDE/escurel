import { personIcon, typeIcon } from './node-icons';
import { formatAge } from '../../src/shared/time';
import { describeNodeType } from '../../src/thread/nodeStyle';
import { MARGIN, MAX_LISTED_DRAFTS } from '../../src/thread/layout';
import { LitElement, css, html, nothing, svg } from 'lit';
import type { PropertyValues } from 'lit';
import { property, state } from 'lit/decorators.js';
import type {
  FocusGraph,
  LaidOutNode,
  ThreadLayout,
  ThreadNode,
  ThreadView,
  ThreadWebviewToHost,
} from '../../src/shared/protocol';
import { theme } from '../shared/theme.css';
import { fitToBounds, panToReveal, zoomAboutPoint } from './viewport';
import { chipWords } from '../../src/thread/chipWords';
import { checkIcon, clockIcon, crossIcon, syncIcon } from '../shared/icons';
import {
  columnsOffRight,
  firstViewport,
  isLowZoom,
  pickTarget,
  scrollMetrics,
} from '../../src/thread/firstView';
import type { ViewportState } from './viewport';

/** Where a fitted graph starts: just under the 28px pinned column headers. */
const FIT_TOP = 40;

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
      .zoom-hint {
        font-size: 0.85em;
        color: var(--escurel-muted);
        border: 1px solid var(--escurel-border);
        border-radius: 8px;
        padding: 0 6px;
        text-transform: uppercase;
        letter-spacing: 0.04em;
      }
      /* The icon of a state chip: small, inline with the word, never taller than the chip. */
      .card .chip {
        display: inline-flex;
        align-items: center;
        gap: 3px;
        line-height: 1.5;
      }
      .card .chip svg {
        flex: none;
        width: 10px;
        height: 10px;
      }
      /* More stages lie beyond the right edge: a fade and a chip that brings them in. */
      .edge-fade {
        position: absolute;
        top: 0;
        right: 0;
        bottom: 8px;
        width: 56px;
        z-index: 5;
        pointer-events: none;
        background: linear-gradient(to right, transparent, var(--vscode-editor-background));
      }
      button.edge-more {
        position: absolute;
        z-index: 7;
        right: 18px;
        bottom: 20px;
        padding: 4px 10px;
        font: inherit;
        font-size: 0.85em;
        color: var(--vscode-button-secondaryForeground);
        background: var(--vscode-button-secondaryBackground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        border-radius: 11px;
        cursor: pointer;
      }
      button.edge-more:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
        outline-offset: 1px;
      }
      .scroll-track {
        position: absolute;
        z-index: 6;
        background: color-mix(
          in srgb,
          var(--vscode-scrollbarSlider-background, var(--escurel-muted)) 15%,
          transparent
        );
      }
      .scroll-track.h {
        left: 0;
        right: 0;
        bottom: 0;
        height: 8px;
      }
      .scroll-track.v {
        top: 0;
        right: 0;
        bottom: 0;
        width: 8px;
      }
      .scroll-thumb {
        box-sizing: border-box;
        position: absolute;
        background: var(--vscode-scrollbarSlider-background, var(--escurel-muted));
        border: 1px solid var(--vscode-contrastBorder, transparent);
        border-radius: 4px;
        cursor: grab;
      }
      .scroll-thumb:hover {
        background: var(--vscode-scrollbarSlider-hoverBackground, var(--escurel-muted));
      }
      .scroll-thumb.h {
        top: 0;
        bottom: 0;
      }
      .scroll-thumb.v {
        left: 0;
        right: 0;
      }
      /* Semantic zoom: below 70% a card keeps its icon, type word, title and state chip, and drops
         the body (subtitle, meta, reason, draft list, buttons). The words are COUNTER-SCALED: the
         canvas shrinks by --zoom, so their font is divided by it and they render at about 10.5px
         whatever the zoom. The box keeps its size, so wires and positions do not move; the full
         text stays in the tooltip and the accessible name. A title that does not fit is cut with an
         ellipsis, never over a neighbour. */
      .low-zoom .card,
      .low-zoom .card.compact {
        --lz: calc(10.5px / var(--zoom, 0.5));
        justify-content: center;
        gap: 0;
        padding-top: 0;
        padding-bottom: 0;
      }
      /* A row never shrinks: squeezed, it clipped its own text while its box still looked inside
         the card. Line height 1 lets two counter-scaled rows fit the smallest card at 40%. */
      .low-zoom .card > * {
        flex-shrink: 0;
      }
      .low-zoom .card .card-subtitle,
      .low-zoom .card .meta-lines,
      .low-zoom .card .needs-reason,
      .low-zoom .card .needs-text,
      .low-zoom .card .changeset-author,
      .low-zoom .card .draft-list,
      .low-zoom .card .gate-actions,
      .low-zoom .card .collapse-toggle {
        display: none;
      }
      .low-zoom .card .card-title,
      .low-zoom .card .type-label,
      .low-zoom .card .chip,
      .low-zoom .card .needs-badge {
        font-size: var(--lz);
        line-height: 1;
      }
      .low-zoom .card .type-icon svg {
        width: var(--lz);
        height: var(--lz);
      }
      .low-zoom .card .chip,
      .low-zoom .card .type-label {
        /* At counter-scaled size a word can be wider than the card: cut it, never spill over. */
        min-width: 0;
        flex: 0 1 auto;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .low-zoom .card .chip {
        padding: 0 calc(var(--lz) * 0.5);
        border-width: 0;
      }
      /* The type word is what the colour and icon stand for, so it keeps its full width; the chip
         (whose full text is in the tooltip) gives way first. */
      .low-zoom .card .type-label {
        flex: 0 0 auto;
      }
      .low-zoom .card .chip {
        flex: 0 1 auto;
        min-width: 0;
      }
      .low-zoom .card .card-header,
      .low-zoom .card .type-line,
      .low-zoom .card .compact-line,
      .low-zoom .card .card-footer,
      .low-zoom .card .needs-row {
        flex-wrap: nowrap;
        margin: 0;
        line-height: 1;
        gap: calc(var(--lz) * 0.4);
        min-width: 0;
        overflow: hidden;
      }
      /* The canvas scales a 2px outline to 0.8px at 40%: the ring that says where the keyboard is
         counter-scales like the words, so it renders at about 2px at any zoom (Chrome floors an outline to whole pixels, hence 2.5). */
      .low-zoom .card:focus-visible {
        outline-width: calc(2.5px / var(--zoom, 0.5));
        outline-offset: calc(1px / var(--zoom, 0.5));
      }
      .low-zoom .card.selected {
        box-shadow: 0 0 0 calc(2px / var(--zoom, 0.5)) var(--vscode-focusBorder);
      }
      .low-zoom .card.needs-you.selected {
        box-shadow:
          0 0 0 calc(3px / var(--zoom, 0.5)) color-mix(in srgb, var(--accent) 24%, transparent),
          0 0 0 calc(5px / var(--zoom, 0.5)) var(--vscode-focusBorder);
      }
      .low-zoom .card .card-title {
        flex: 1;
        min-width: 0;
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
        font-size: 0.85em;
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
        /* The theme's widget border nearly vanished on the canvas once the cards had real borders. */
        stroke: var(
          --vscode-contrastBorder,
          color-mix(in srgb, var(--vscode-foreground) 40%, transparent)
        );
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
        /* The theme's widget border was one or two levels off the background in light and dark, so
           a card lost its edge. A tint of the foreground keeps it visible; high contrast keeps its own. */
        border: 1px solid
          var(
            --vscode-contrastBorder,
            color-mix(in srgb, var(--vscode-foreground) 32%, transparent)
          );
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
      .type-icon {
        display: inline-flex;
        flex-shrink: 0;
        color: var(--accent, var(--escurel-muted));
      }
      .type-qualifier {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-weight: 600;
        color: var(--vscode-foreground);
      }
      .low-zoom .card .type-qualifier {
        display: none;
      }
      .type-label {
        font-size: 0.8em;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.06em;
        color: var(--accent, var(--escurel-muted));
      }
      .type-line {
        display: flex;
        gap: 6px;
        align-items: baseline;
        min-width: 0;
      }
      .type-line .card-subtitle {
        flex: 1;
        min-width: 0;
      }
      /* Each type has its own accent colour, drawn as a bar on the card's left edge. The icon and
         the type word say the same thing, so colour is never the only cue. */
      .type-event {
        --accent: var(--escurel-event);
      }
      .type-cascade {
        --accent: var(--vscode-charts-yellow, var(--escurel-event));
      }
      .type-run {
        --accent: var(--escurel-run);
      }
      .type-changeset {
        --accent: var(--escurel-skill);
      }
      .type-page {
        --accent: var(--escurel-instance);
      }
      .card {
        border-left: 4px solid var(--accent, var(--escurel-border));
      }
      .card.compact {
        padding: 4px 10px;
        justify-content: center;
        gap: 3px;
        background: transparent;
      }
      .card.compact .card-title {
        font-weight: 500;
      }
      .compact-line {
        display: flex;
        align-items: center;
        gap: 6px;
        min-width: 0;
      }
      /* Work that waits on a person: a strong accent in the warning colour, a halo (static, no
         animation) and a badge. The badge and the reason are words, so it is not colour alone. */
      .card.needs-you {
        --accent: var(--vscode-editorWarning-foreground, var(--escurel-event));
        border: 2px solid var(--accent);
        border-left-width: 6px;
        background: color-mix(
          in srgb,
          var(--accent) 8%,
          var(--vscode-editorWidget-background, var(--vscode-editor-background))
        );
        box-shadow:
          0 0 0 3px color-mix(in srgb, var(--accent) 24%, transparent),
          0 2px 8px var(--vscode-widget-shadow, transparent);
      }
      .card.needs-you.selected {
        /* Selection is a ring BEYOND the halo: the warning border and halo stay. */
        border-color: var(--accent);
        box-shadow:
          0 0 0 3px color-mix(in srgb, var(--accent) 24%, transparent),
          0 0 0 5px var(--vscode-focusBorder);
      }
      .needs-row {
        display: flex;
        align-items: center;
        gap: 8px;
        min-width: 0;
      }
      .needs-badge {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        flex-shrink: 0;
        padding: 1px 8px 1px 6px;
        border-radius: 10px;
        border: 1px solid var(--accent);
        background: color-mix(in srgb, var(--accent) 30%, transparent);
        color: var(--vscode-foreground);
        font-size: 0.85em;
        font-weight: 700;
      }
      .needs-reason {
        font-size: 0.85em;
        color: var(--vscode-foreground);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .changeset-author {
        font-size: 0.85em;
        color: var(--escurel-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }
      .draft-list {
        display: flex;
        flex-direction: column;
        gap: 2px;
        margin: 2px 0;
      }
      .draft-entry {
        all: unset;
        box-sizing: border-box;
        display: block;
        height: 20px;
        line-height: 20px;
        padding: 0 4px;
        font-size: 0.85em;
        color: var(--vscode-textLink-foreground);
        cursor: pointer;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }
      .draft-entry:hover {
        text-decoration: underline;
      }
      .draft-entry:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
      }
      .draft-more {
        all: unset;
        box-sizing: border-box;
        display: block;
        height: 20px;
        line-height: 20px;
        padding: 0 4px;
        font-size: 0.85em;
        color: var(--vscode-textLink-foreground);
        cursor: pointer;
      }
      .draft-more:hover {
        text-decoration: underline;
      }
      .draft-more:focus-visible {
        outline: 1px solid var(--vscode-focusBorder);
      }
      .review-btn {
        background: var(--vscode-button-secondaryBackground);
        color: var(--vscode-button-secondaryForeground, var(--vscode-foreground));
        border: 1px solid var(--vscode-button-border, var(--escurel-border));
        font-size: 0.85em;
        padding: 1px 6px;
        border-radius: 2px;
      }
      .gate-actions button[aria-disabled='true'] {
        opacity: 0.55;
        cursor: not-allowed;
      }
      .gate-reason {
        flex-basis: 100%;
        font-size: 0.85em;
        color: var(--escurel-muted);
      }
      .gate-actions {
        flex-wrap: wrap;
      }
      .lane-divider {
        position: absolute;
        left: 0;
        height: 0;
        border-top: 1px dashed
          var(
            --vscode-contrastBorder,
            color-mix(in srgb, var(--vscode-foreground) 30%, transparent)
          );
        pointer-events: none;
      }
      .lane-caption {
        position: absolute;
        font-size: 0.85em;
        color: var(--escurel-muted);
        white-space: nowrap;
        pointer-events: none;
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
        font-size: 0.85em;
        color: var(--escurel-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .collapse-toggle {
        font-size: 0.85em;
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
      .meta-line:first-child {
        color: var(--vscode-foreground);
      }
      .meta-lines {
        display: flex;
        flex-direction: column;
        gap: 2px;
        margin: 2px 0;
      }
      .meta-line {
        /* 11px at the editor's 13px: the cards were unreadable at 9.75px in dim grey. */
        font-size: 0.85em;
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
        flex-wrap: wrap;
        align-items: center;
        gap: 8px;
      }
      .gate-actions button {
        min-height: 24px;
        box-sizing: border-box;
        cursor: pointer;
      }
      .promote-btn {
        /* charts.green is a LIGHT green in dark and high-contrast themes; white on it was 1.8:1. */
        background: color-mix(in srgb, var(--escurel-run) 55%, black);
        color: var(--vscode-button-foreground);
        border: 1px solid transparent;
        font-size: 0.85em;
        padding: 3px 10px;
        border-radius: 2px;
      }
      .approve-btn {
        background: var(--vscode-button-background);
        color: var(--vscode-button-foreground);
        border: 1px solid var(--vscode-button-border, transparent);
        font-size: 0.85em;
        padding: 3px 10px;
        border-radius: 2px;
      }
      .retry-btn {
        background: var(--vscode-button-secondaryBackground);
        color: var(--vscode-button-secondaryForeground);
        border: 1px solid var(--vscode-button-border, var(--vscode-contrastBorder, transparent));
        font-size: 0.85em;
        padding: 3px 10px;
        border-radius: 2px;
      }
      .discard-btn {
        /* Quiet on purpose: available, never competing with Promote. No fill, no border (the
           wording and the red text are the cue), underlined on hover and focus. */
        background: transparent;
        color: var(--vscode-errorForeground);
        border: 1px solid transparent;
        font-size: 0.85em;
        padding: 3px 8px;
        border-radius: 2px;
      }
      .discard-btn:hover,
      .discard-btn:focus-visible {
        text-decoration: underline;
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
    `,
  ];

  @property({ attribute: false }) view?: ThreadView;
  @property({ attribute: false }) layout?: ThreadLayout;
  // @ts-expect-error Specification requires property named 'focus' for FocusGraph
  @property({ attribute: false }) override focus?: FocusGraph;
  @property({ attribute: false }) error?: { message: string; canReconnect: boolean };
  /** The clock a card's age is measured against; a visual baseline pins it so it does not age. */
  @property({ attribute: false }) clock: () => Date = () => new Date();

  @state() focusedNodeId = '';
  @state() selectedNodeId = '';
  @state() hoveredNodeId = '';
  @state() viewport: ViewportState = { x: 0, y: 0, zoom: 1.0 };
  @state() private isPanning = false;

  /** The first view of a thread is applied once; after that the viewport belongs to the person. */
  private firstViewApplied = false;
  /** The canvas area's size, for the scrollbar thumbs; kept by a ResizeObserver. */
  @state() private areaSize = { width: 0, height: 0 };
  private resizeObserver?: ResizeObserver;
  private thumbDrag?: { axis: 'h' | 'v'; start: number; origin: number };
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

  protected override updated(): void {
    const area = this.shadowRoot?.querySelector<HTMLElement>('.canvas-area') ?? undefined;
    this.observeArea(area);
    if (this.firstViewApplied || !this.layout || !this.view) return;
    if (!area || !area.clientWidth) return; // not measurable yet (a hidden tab): the observer retries
    this.firstViewApplied = true;
    // A graph that fits opens as it is. A bigger one opens at 100% with the node that matters (the
    // first that needs you, else the root) at the left, and a scrollbar to reach the rest.
    // Fitting everything used to shrink big threads to 40-50%, where no card text was legible; Fit
    // still gives that overview on request.
    const viewport = firstViewport(this.layout, pickTarget(this.view, this.layout), {
      width: area.clientWidth,
      height: area.clientHeight,
    });
    // After this update: changing state inside `updated` is a Lit dev-mode warning.
    queueMicrotask(() => {
      this.viewport = viewport;
    });
  }

  /**
   * Keep `areaSize` current (for the scrollbar thumbs and for a first view that had to wait for a
   * measurable canvas). The canvas element is replaced when the thread goes empty and comes back, so
   * the observer follows the element, not the first one it saw. State is changed in a microtask:
   * changing it inside `updated` is a Lit dev-mode warning.
   */
  private observedArea?: HTMLElement;
  private observeArea(area: HTMLElement | undefined): void {
    if (area === this.observedArea) return;
    this.resizeObserver?.disconnect();
    this.observedArea = area;
    if (!area) return;
    const measure = () => {
      const a = this.observedArea;
      if (!a) return;
      if (a.clientWidth !== this.areaSize.width || a.clientHeight !== this.areaSize.height) {
        this.areaSize = { width: a.clientWidth, height: a.clientHeight };
      }
    };
    queueMicrotask(measure);
    if (typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(measure);
      this.resizeObserver.observe(area);
    }
  }

  override disconnectedCallback(): void {
    this.resizeObserver?.disconnect();
    this.resizeObserver = undefined;
    this.observedArea = undefined;
    super.disconnectedCallback();
  }

  public selectNode(nodeId: string): void {
    this.selectedNodeId = nodeId;
    this.focusedNodeId = nodeId;
    // The details are a view of their own now (the panel area), so selecting takes no width from
    // the canvas and one reveal is enough.
    this.panToFocusedNode();
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

  /** A state chip: the icon's shape and a short word, so state is never colour alone. */
  private renderChip(chip: { text: string; tone: string }) {
    const words = chipWords(chip.text);
    const icon =
      words.icon === 'check'
        ? checkIcon()
        : words.icon === 'cross'
          ? crossIcon()
          : words.icon === 'sync'
            ? syncIcon()
            : words.icon === 'clock'
              ? clockIcon()
              : nothing;
    return html`<span class="chip ${chip.tone}" title=${chip.text}>${icon}${words.text}</span>`;
  }

  public fit(): void {
    if (!this.layout) return;
    const containerEl = this.shadowRoot?.querySelector('.canvas-area');
    const containerSize = {
      width: containerEl?.clientWidth || 800,
      height: containerEl?.clientHeight || 600,
    };
    // Never above 100%: a small thread fitted to a big pane would blow every card up past readable.
    const fitted = fitToBounds(this.layout.bounds, containerSize, 20, 1);
    // Start under the pinned column headers: a graph shorter than the pane used to float in the
    // middle with an empty band above it.
    this.viewport = { ...fitted, y: Math.min(fitted.y, FIT_TOP) };
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

  /** The level in the lineage tree (the root is 1), not the stage column. */
  private treeLevel(nodeId: string): number {
    const parentOf = new Map(this.view?.nodes.map((n) => [n.id, n.parent]));
    let level = 1;
    const seen = new Set<string>();
    for (let p = parentOf.get(nodeId); p && !seen.has(p); p = parentOf.get(p)) {
      seen.add(p);
      level += 1;
    }
    return level;
  }

  /** Who proposed an open changeset, when, and the pages it changes, each one openable. */
  private renderChangesetDetails(node: ThreadNode) {
    const details = node.changeset;
    if (!details || node.emphasis !== 'needs-you') return nothing;
    const age = formatAge(details.at, this.clock());
    const who = [details.author, age].filter(Boolean).join(' · ');
    const listed = details.drafts.slice(0, MAX_LISTED_DRAFTS);
    const more = details.drafts.length - listed.length;
    return html`
      ${who ? html`<div class="changeset-author" title="${details.at ?? ''}">${who}</div>` : nothing}
      <div class="draft-list">
        ${listed.map(
          (draft) =>
            html`<button
              class="draft-entry"
              title="Open ${draft.title}"
              @click=${(e: Event) => {
                e.stopPropagation();
                this.send({ type: 'open-node', nodeId: draft.id });
              }}
            >
              ${draft.title}
            </button>`,
        )}
        ${
          more > 0
            ? html`<button
                class="draft-more"
                title="Open the whole changeset"
                @click=${(e: Event) => {
                  e.stopPropagation();
                  this.send({ type: 'open-node', nodeId: node.id });
                }}
              >
                +${more} more
              </button>`
            : nothing
        }
      </div>
    `;
  }

  /** Thin scrollbars on the axes where the graph is bigger than the canvas: the cut-off edge is reachable. */
  private renderScrollbars() {
    if (!this.layout) return nothing;
    const m = scrollMetrics(this.viewport, this.layout.bounds, this.areaSize);
    const thumb = (axis: 'h' | 'v', t: { size: number; pos: number }) => {
      const style =
        axis === 'h'
          ? `left: ${t.pos * 100}%; width: ${t.size * 100}%;`
          : `top: ${t.pos * 100}%; height: ${t.size * 100}%;`;
      return html`<div
        class="scroll-track ${axis}"
        aria-hidden="true"
        @pointerdown=${(e: PointerEvent) => e.stopPropagation()}
      >
        <div
          class="scroll-thumb ${axis}"
          style=${style}
          @pointerdown=${(e: PointerEvent) => this.startThumbDrag(e, axis)}
          @pointermove=${(e: PointerEvent) => this.moveThumbDrag(e)}
          @pointerup=${() => (this.thumbDrag = undefined)}
          @pointercancel=${() => (this.thumbDrag = undefined)}
        ></div>
      </div>`;
    };
    const off = columnsOffRight(this.layout, this.viewport, this.areaSize.width);
    return html`${m.h ? thumb('h', m.h) : nothing}${m.v ? thumb('v', m.v) : nothing}${
      off > 0
        ? html`<div class="edge-fade" aria-hidden="true"></div>
            <button
              class="edge-more"
              title="Show the rest of the thread"
              @pointerdown=${(e: PointerEvent) => e.stopPropagation()}
              @click=${() => this.panRight()}
            >
              ${off} more ${off === 1 ? 'stage' : 'stages'} →
            </button>`
        : nothing
    }`;
  }

  /** One click on the 'more stages' chip: bring the next stretch of the graph in. */
  private panRight(): void {
    if (!this.layout) return;
    const width = this.areaSize.width || 800;
    const min = width - this.layout.bounds.width * this.viewport.zoom;
    this.viewport = {
      ...this.viewport,
      x: Math.max(min, this.viewport.x - Math.round(width * 0.8)),
    };
  }

  private startThumbDrag(e: PointerEvent, axis: 'h' | 'v'): void {
    e.stopPropagation();
    this.thumbDrag = {
      axis,
      start: axis === 'h' ? e.clientX : e.clientY,
      origin: axis === 'h' ? this.viewport.x : this.viewport.y,
    };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  private moveThumbDrag(e: PointerEvent): void {
    const drag = this.thumbDrag;
    if (!drag || !this.layout) return;
    const trackLength = drag.axis === 'h' ? this.areaSize.width : this.areaSize.height;
    const extent =
      (drag.axis === 'h' ? this.layout.bounds.width : this.layout.bounds.height) *
      this.viewport.zoom;
    if (trackLength <= 0) return;
    // A thumb moves trackLength/extent of what the content moves.
    const delta =
      ((drag.axis === 'h' ? e.clientX : e.clientY) - drag.start) * (extent / trackLength);
    const min = Math.min(0, trackLength - extent);
    const next = Math.max(min, Math.min(0, drag.origin - delta));
    this.viewport =
      drag.axis === 'h' ? { ...this.viewport, x: next } : { ...this.viewport, y: next };
  }

  private renderCard(node: ThreadNode, layoutNode: LaidOutNode, lowZoom: boolean) {
    const isFocused = node.id === this.focusedNodeId;
    const isSelected = node.id === this.selectedNodeId;
    const described = describeNodeType(node, this.view?.rootEventId ?? '');
    const compact = node.emphasis === 'compact';
    const needs = node.needsYou;
    const accessibleName = [
      `${described.label}: ${node.title}${described.qualifier ? ` (${described.qualifier})` : ''}`,
      node.subtitle,
      node.state,
      needs ? `needs you: ${needs.text}` : undefined,
    ]
      .filter(Boolean)
      .join(', ');
    // A small card hides its details; they stay on hover and in the inspector.
    const tooltip =
      compact || lowZoom
        ? [
            `${described.label}: ${node.title}${described.qualifier ? ` (${described.qualifier})` : ''}`,
            node.subtitle,
            node.state,
            needs?.text,
            ...node.meta,
          ]
            .filter(Boolean)
            .join('\n')
        : nothing;
    const subtitle = node.subtitle && node.subtitle !== described.label ? node.subtitle : undefined;

    // Check if any child is hidden by collapse to derive expansion state.
    const isCollapsed = node.children.some((childId) => {
      const childLayout = this.layout?.nodes.find((n) => n.id === childId);
      return childLayout?.hidden === true;
    });

    return html`
      <div
        class="card type-${described.type} ${compact ? 'compact' : ''} ${needs ? 'needs-you' : ''} ${isSelected ? 'selected' : ''}"
        data-node-id="${node.id}"
        role="treeitem"
        aria-level="${this.treeLevel(node.id)}"
        aria-selected="${isSelected ? 'true' : 'false'}"
        aria-label="${accessibleName}"
        title=${tooltip}
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
          <span class="type-icon">${typeIcon(described.type)}</span>
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
          compact
            ? html`<div class="compact-line">
                <span class="type-label">${described.label}</span>
                ${
                  described.qualifier
                    ? html`<span class="type-qualifier" title=${described.qualifier}
                        >${described.qualifier}</span
                      >`
                    : nothing
                }
                ${node.chips.map((chip) => this.renderChip(chip))}
              </div>`
            : html`<div class="type-line">
                  <span class="type-label">${described.label}</span>
                  ${
                    described.qualifier
                      ? html`<span class="type-qualifier" title=${described.qualifier}
                          >${described.qualifier}</span
                        >`
                      : nothing
                  }
                  ${
                    subtitle
                      ? html`<span class="card-subtitle" title="${subtitle}">${subtitle}</span>`
                      : nothing
                  }
                </div>
                ${
                  needs
                    ? html`<div class="needs-row">
                        <span class="needs-badge"
                          >${personIcon()}<span class="needs-text">Needs you</span></span
                        >
                        <span class="needs-reason" title="${needs.text}">${needs.text}</span>
                      </div>`
                    : nothing
                }
                ${this.renderChangesetDetails(node)}
                ${
                  node.meta.length && !node.changeset?.drafts.length
                    ? html`<div class="meta-lines">
                        ${node.meta
                          .slice(0, 4)
                          .map((line) => html`<div class="meta-line">${line}</div>`)}
                      </div>`
                    : nothing
                }`
        }
        ${
          compact
            ? nothing
            : html`<div class="card-footer">
                ${node.chips.map((chip) => this.renderChip(chip))}
                ${
                  needs?.reason === 'approve-plan'
                    ? html`<div class="gate-actions">
                        <button
                          class="approve-btn"
                          @click=${(e: Event) => {
                            e.stopPropagation();
                            this.send({ type: 'run-control', action: 'approve', runId: node.id });
                          }}
                        >
                          Approve plan
                        </button>
                      </div>`
                    : needs?.reason === 'failed'
                      ? html`<div class="gate-actions">
                          <button
                            class="retry-btn"
                            @click=${(e: Event) => {
                              e.stopPropagation();
                              this.send({ type: 'run-control', action: 'retry', runId: node.id });
                            }}
                          >
                            Retry
                          </button>
                        </div>`
                      : nothing
                }
                ${
                  node.gate
                    ? html`<div class="gate-actions">
                        <button
                          class="promote-btn"
                          aria-disabled=${node.gate.disabledReason ? 'true' : nothing}
                          @click=${(e: Event) => {
                            e.stopPropagation();
                            if (node.gate?.disabledReason) return;
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
                          aria-disabled=${node.gate.disabledReason ? 'true' : nothing}
                          @click=${(e: Event) => {
                            e.stopPropagation();
                            if (node.gate?.disabledReason) return;
                            this.send({
                              type: 'discard',
                              changesetId: node.gate?.changesetId,
                              draftId: node.gate?.draftId,
                            });
                          }}
                        >
                          Discard
                        </button>
                        ${
                          node.kind === 'changeset'
                            ? html`<button
                                class="review-btn"
                                @click=${(e: Event) => {
                                  e.stopPropagation();
                                  this.send({ type: 'open-node', nodeId: node.id });
                                }}
                              >
                                Review changes
                              </button>`
                            : nothing
                        }
                        ${
                          node.gate.disabledReason
                            ? html`<span class="gate-reason">${node.gate.disabledReason}</span>`
                            : nothing
                        }
                      </div>`
                    : nothing
                }
              </div>`
        }
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

    const lowZoom = isLowZoom(this.viewport.zoom);
    const nodeMap = new Map<string, ThreadNode>(this.view.nodes.map((n) => [n.id, n]));
    const visibleLaidOutNodes = this.layout.nodes.filter((n) => !n.hidden);

    return html`
      <div class="toolbar" role="group" aria-label="Thread canvas controls">
        <button aria-label="Zoom out" @click=${() => this.zoomBy(0.8)}>−</button>
        <span class="zoom-level" aria-live="polite">${Math.round(this.viewport.zoom * 100)}%</span>
        ${lowZoom ? html`<span class="zoom-hint" title="Zoomed out: cards show icon, type, title and state, without their details. Zoom in to 70% for the details.">overview</span>` : nothing}
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
          class="canvas-area ${this.isPanning ? 'panning' : ''} ${lowZoom ? 'low-zoom' : ''}"
          style="--zoom: ${this.viewport.zoom}"
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
              style="transform: translateX(${this.viewport.x}px);"
            >
              ${this.layout.columnHeaders.map(
                (col) =>
                  html`<div class="column-header" style="left: ${col.x * this.viewport.zoom}px;">
                    ${col.label}
                  </div>`,
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

            ${this.layout.lanes
              .filter((lane) => lane.index > 0)
              .map(
                (lane) => html`
                  <div
                    class="lane-divider"
                    aria-hidden="true"
                    style="top: ${lane.y - 6}px; width: ${this.layout!.bounds.width}px;"
                  ></div>
                  <div
                    class="lane-caption"
                    aria-hidden="true"
                    style="top: ${lane.y - 24}px; left: ${MARGIN}px;"
                  >
                    follow-up${lane.title ? html` · ${lane.title}` : nothing}
                  </div>
                `,
              )}
            ${visibleLaidOutNodes.map((laidOut) => {
              const node = nodeMap.get(laidOut.id);
              return node ? this.renderCard(node, laidOut, lowZoom) : nothing;
            })}
          </div>
          ${this.renderScrollbars()}
        </div>
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
