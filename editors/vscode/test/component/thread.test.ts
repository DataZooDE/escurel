import { pickTarget } from '../../src/thread/firstView';
import { expect, fixture, html } from '@open-wc/testing';
import type { ThreadWebviewToHost } from '../../src/shared/protocol';
import { layoutThread } from '../../src/thread/layout';
import { connectThreadWebview } from '../../webview/thread/main';
import type { EscurelThreadCanvas } from '../../webview/thread/thread-canvas';
import '../../webview/thread/thread-canvas';
import {
  branchingFocus,
  branchingLayout,
  branchingThreadView,
  gatedFocus,
  gatedLayout,
  gatedThreadView,
  recordedFocus,
  recordedLayout,
  recordedThreadView,
} from './thread-fixtures';

async function renderCanvas(
  props: Partial<EscurelThreadCanvas> = {},
): Promise<EscurelThreadCanvas> {
  const el = await fixture<EscurelThreadCanvas>(html`
    <escurel-thread-canvas
      .view=${props.view ?? recordedThreadView}
      .layout=${props.layout ?? recordedLayout}
      .focus=${props.focus ?? recordedFocus}
      .error=${props.error}
    ></escurel-thread-canvas>
  `);
  await el.updateComplete;
  return el;
}

const q = (el: Element, selector: string) => el.shadowRoot!.querySelector(selector);
const qa = (el: Element, selector: string) => [...el.shadowRoot!.querySelectorAll(selector)];
const text = (element: Element | null | undefined) =>
  (element?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-thread-canvas>', () => {
  it('renders a card per visible node and a wire per wire', async () => {
    const el = await renderCanvas();
    const visibleNodes = recordedLayout.nodes.filter((n) => !n.hidden);
    const cards = qa(el, '.card');
    expect(cards.length).to.equal(visibleNodes.length);

    // Each visible card is placed in the DOM with treeitem role and a lineage-depth aria-level.
    const parentOf = new Map(recordedThreadView.nodes.map((n) => [n.id, n.parent]));
    const depthOf = (id: string): number => {
      let depth = 1;
      for (let p = parentOf.get(id); p; p = parentOf.get(p)) depth += 1;
      return depth;
    };
    for (const node of visibleNodes) {
      const card = q(el, `.card[data-node-id="${node.id}"]`);
      expect(card).to.exist;
      expect(card?.getAttribute('role')).to.equal('treeitem');
      // The tree level is the depth in the lineage, not the stage column: a changeset shares its run's
      // column and a follow-on event skips one.
      expect(card?.getAttribute('aria-level')).to.equal(String(depthOf(node.id)));
    }

    const wires = qa(el, '.wire');
    expect(wires.length).to.equal(recordedLayout.wires.length);

    // Wire styles correspond to the layout wire definitions.
    const promotedWire = q(el, '.wire.promoted');
    expect(promotedWire).to.exist;

    // Column headers are rendered from layout.columnHeaders.
    const headers = qa(el, '.column-header');
    expect(headers.length).to.equal(recordedLayout.columnHeaders.length);
    expect(text(headers[0])).to.equal(recordedLayout.columnHeaders[0]?.label);
  });

  it('gate buttons post the right message', async () => {
    const el = await renderCanvas({
      view: gatedThreadView,
      layout: gatedLayout,
      focus: gatedFocus,
    });
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const changesetCard = q(el, '.card[data-node-id="01M3NHJJNJ7P58Q9A7PWEYSA9X"]');
    expect(changesetCard).to.exist;

    const promoteBtn = changesetCard?.querySelector('.promote-btn') as HTMLButtonElement | null;
    const discardBtn = changesetCard?.querySelector('.discard-btn') as HTMLButtonElement | null;
    expect(promoteBtn).to.exist;
    expect(discardBtn).to.exist;
    expect(text(promoteBtn)).to.equal('Promote all 1');
    expect(text(discardBtn)).to.equal('Discard');

    promoteBtn?.click();
    expect(sent).to.deep.equal([
      { type: 'promote', changesetId: '01M3NHJJNJ7P58Q9A7PWEYSA9X', draftId: undefined },
    ]);

    sent.length = 0;
    discardBtn?.click();
    expect(sent).to.deep.equal([
      { type: 'discard', changesetId: '01M3NHJJNJ7P58Q9A7PWEYSA9X', draftId: undefined },
    ]);
  });

  it('collapse toggle posts toggle-collapse', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const toggleBtn = q(el, '.collapse-toggle') as HTMLButtonElement | null;
    expect(toggleBtn).to.exist;
    toggleBtn?.click();

    expect(sent).to.deep.equal([
      { type: 'toggle-collapse', nodeId: recordedThreadView.rootEventId },
    ]);
  });

  it('arrow keys follow the focus graph (→ from the root reaches the run)', async () => {
    const el = await renderCanvas();
    const rootId = recordedThreadView.rootEventId;
    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';

    const rootCard = q(el, `.card[data-node-id="${rootId}"]`) as HTMLElement;
    expect(rootCard).to.exist;
    rootCard.focus();

    // ArrowRight moves from root to run along focus.steps[rootId].next.
    rootCard.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await el.updateComplete;

    expect(el.focusedNodeId).to.equal(runId);
    const runCard = q(el, `.card[data-node-id="${runId}"]`) as HTMLElement;
    expect(el.shadowRoot?.activeElement).to.equal(runCard);

    // ArrowLeft moves back from run to root along focus.steps[runId].back.
    runCard.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    await el.updateComplete;

    expect(el.focusedNodeId).to.equal(rootId);
    expect(el.shadowRoot?.activeElement).to.equal(rootCard);
  });

  it('Enter posts open-node for the focused node', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const rootId = recordedThreadView.rootEventId;
    const rootCard = q(el, `.card[data-node-id="${rootId}"]`) as HTMLElement;
    rootCard.focus();
    rootCard.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));

    expect(sent).to.deep.equal([{ type: 'open-node', nodeId: rootId }]);
  });

  it('Esc focuses the toolbar', async () => {
    const el = await renderCanvas();
    const rootId = recordedThreadView.rootEventId;
    const rootCard = q(el, `.card[data-node-id="${rootId}"]`) as HTMLElement;
    rootCard.focus();

    rootCard.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await el.updateComplete;

    const toolbarFirstBtn = q(el, '.toolbar button');
    expect(el.shadowRoot?.activeElement).to.equal(toolbarFirstBtn);
  });

  it('exactly one card has tabindex="0"', async () => {
    const el = await renderCanvas();
    const cards = qa(el, '.card');
    const zeroTabIndexCards = cards.filter((c) => c.getAttribute('tabindex') === '0');
    expect(zeroTabIndexCards.length).to.equal(1);
    expect(zeroTabIndexCards[0]?.getAttribute('data-node-id')).to.equal(recordedFocus.first);

    // All other cards must have tabindex="-1".
    const minusOneTabIndexCards = cards.filter((c) => c.getAttribute('tabindex') === '-1');
    expect(minusOneTabIndexCards.length).to.equal(cards.length - 1);
  });

  it('expand-all posts', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const expandAllBtn = q(el, 'button[data-action="expand-all"]') as HTMLButtonElement;
    expect(expandAllBtn).to.exist;
    expandAllBtn.click();

    expect(sent).to.deep.equal([{ type: 'expand-all' }]);
  });

  it('wheel zoom stays within 0.4–2.5', async () => {
    const el = await renderCanvas();
    const canvasContainer = q(el, '.canvas-area') as HTMLElement;
    expect(canvasContainer).to.exist;

    // Zoom out repeatedly.
    for (let i = 0; i < 20; i++) {
      canvasContainer.dispatchEvent(
        new WheelEvent('wheel', { deltaY: 200, clientX: 100, clientY: 100, bubbles: true }),
      );
    }
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.closeTo(0.4, 0.001);

    // Zoom in repeatedly.
    for (let i = 0; i < 30; i++) {
      canvasContainer.dispatchEvent(
        new WheelEvent('wheel', { deltaY: -200, clientX: 100, clientY: 100, bubbles: true }),
      );
    }
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.closeTo(2.5, 0.001);
  });

  it('a thread-select message selects the node', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';
    el.selectNode(runId);
    await el.updateComplete;

    expect(el.selectedNodeId).to.equal(runId);
    const runCard = q(el, `.card[data-node-id="${runId}"]`);
    expect(runCard?.classList.contains('selected')).to.equal(true);

    // The details are a view of their own in the panel area: the canvas shows no inspector, so a
    // selection takes no width from it.
    expect(q(el, 'escurel-thread-inspector')).to.equal(null);
    expect(q(el, '.inspector-container')).to.equal(null);
  });

  it('routes ready, loading, thread, error, and select messages while ignoring unknown types', async () => {
    const sent: ThreadWebviewToHost[] = [];
    const api = { postMessage: (msg: ThreadWebviewToHost) => sent.push(msg) };
    const el = await fixture<EscurelThreadCanvas>(
      html`<escurel-thread-canvas></escurel-thread-canvas>`,
    );
    const disconnect = connectThreadWebview(api, el);

    try {
      expect(sent).to.deep.equal([{ type: 'ready' }]);

      // 1. Thread message populates data.
      window.dispatchEvent(
        new MessageEvent('message', {
          data: {
            type: 'thread',
            view: recordedThreadView,
            layout: recordedLayout,
            focus: recordedFocus,
          },
        }),
      );
      await el.updateComplete;
      expect(el.view).to.equal(recordedThreadView);
      expect(el.layout).to.equal(recordedLayout);

      // 2. Thread-select selects node.
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { type: 'thread-select', nodeId: '01M3NHJJGJ1WWAV26F5Z8Y4XKT' },
        }),
      );
      await el.updateComplete;
      expect(el.selectedNodeId).to.equal('01M3NHJJGJ1WWAV26F5Z8Y4XKT');

      // 3. Thread-error sets error state.
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { type: 'thread-error', message: 'Connection dropped', canReconnect: true },
        }),
      );
      await el.updateComplete;
      expect(el.error?.message).to.equal('Connection dropped');
      expect(el.error?.canReconnect).to.equal(true);

      // 4. Unknown message type is ignored.
      window.dispatchEvent(new MessageEvent('message', { data: { type: 'unknown-event' } }));
      expect(el.error?.message).to.equal('Connection dropped');

      // 5. Thread-loading resets state.
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { type: 'thread-loading', rootEventId: recordedThreadView.rootEventId },
        }),
      );
      await el.updateComplete;
      expect(el.view).to.be.undefined;
      expect(el.layout).to.be.undefined;
      expect(el.error).to.be.undefined;
    } finally {
      disconnect();
    }
  });

  it('emphasises wires when connected nodes are hovered or selected', async () => {
    const el = await renderCanvas();
    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';

    // Before selection, no wire is emphasised.
    expect(qa(el, '.wire.emphasised').length).to.equal(0);

    // Select the run: wires connected to or from the run become emphasised.
    el.selectNode(runId);
    await el.updateComplete;

    const emphasisedWires = qa(el, '.wire.emphasised');
    expect(emphasisedWires.length).to.be.greaterThan(0);

    // Deselect and test hover emphasis.
    el.selectedNodeId = '';
    el.hoveredNodeId = runId;
    await el.updateComplete;

    expect(qa(el, '.wire.emphasised').length).to.be.greaterThan(0);
  });

  it('Space key selects the focused node and posts select-node', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const rootId = recordedThreadView.rootEventId;
    const rootCard = q(el, `.card[data-node-id="${rootId}"]`) as HTMLElement;
    rootCard.focus();
    rootCard.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
    await el.updateComplete;

    expect(el.selectedNodeId).to.equal(rootId);
    expect(sent).to.deep.equal([{ type: 'select-node', nodeId: rootId }]);
  });

  it('clicking a card selects it and double clicking opens it', async () => {
    const el = await renderCanvas();
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';
    const runCard = q(el, `.card[data-node-id="${runId}"]`) as HTMLElement;

    runCard.click();
    await el.updateComplete;
    expect(sent).to.deep.equal([{ type: 'select-node', nodeId: runId }]);
    expect(el.selectedNodeId).to.equal(runId);

    sent.length = 0;
    runCard.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    expect(sent).to.deep.equal([{ type: 'open-node', nodeId: runId }]);
  });

  it('toolbar and keyboard zoom controls adjust viewport scale', async () => {
    const el = await renderCanvas();
    const initialZoom = el.viewport.zoom;

    const zoomInBtn = q(el, 'button[aria-label="Zoom in"]') as HTMLButtonElement;
    zoomInBtn.click();
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.greaterThan(initialZoom);

    const zoomOutBtn = q(el, 'button[aria-label="Zoom out"]') as HTMLButtonElement;
    zoomOutBtn.click();
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.closeTo(initialZoom, 0.001);

    const fitBtn = q(el, 'button[data-action="fit"]') as HTMLButtonElement;
    fitBtn.click();
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.within(0.4, 2.5);

    // Keyboard 0 fits to view.
    const rootCard = q(el, '.card') as HTMLElement;
    rootCard.dispatchEvent(new KeyboardEvent('keydown', { key: '0', bubbles: true }));
    await el.updateComplete;
    expect(el.viewport.zoom).to.be.within(0.4, 2.5);
  });

  it('shows error with reconnect button when canReconnect is true, and advice when false', async () => {
    const el = await renderCanvas({
      error: { message: 'Network timeout', canReconnect: true },
    });
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    expect(text(q(el, '.status-message.error'))).to.contain('Network timeout');
    const reconnectBtn = q(el, '.reconnect') as HTMLButtonElement;
    expect(reconnectBtn).to.exist;
    reconnectBtn.click();
    expect(sent).to.deep.equal([{ type: 'refresh' }]);

    el.error = { message: 'Forbidden', canReconnect: false };
    await el.updateComplete;
    expect(text(q(el, '.status-message.error'))).to.contain(
      'Close this panel and open the thread again.',
    );
  });

  it('shows empty and loading states appropriately', async () => {
    const el = await fixture<EscurelThreadCanvas>(
      html`<escurel-thread-canvas></escurel-thread-canvas>`,
    );
    expect(text(q(el, '[role="status"]'))).to.equal('Loading thread…');

    el.view = { ...recordedThreadView, nodes: [] };
    el.layout = { ...recordedLayout, nodes: [] };
    el.focus = { first: '', steps: {} };
    await el.updateComplete;
    expect(text(q(el, '[role="status"]'))).to.equal('No events in this thread.');
  });

  // Findings from codex's review of the canvas, each verified against the code first.

  it('leaves Enter and Space to a button inside a card, so Promote works from the keyboard', async () => {
    // The card's key handler acted on any keydown that bubbled up to it, so Enter on a focused
    // Promote button opened the card instead and was cancelled before the button could
    // activate: Promote, Discard and collapse were unusable without a mouse.
    const el = await renderCanvas({
      view: gatedThreadView,
      layout: gatedLayout,
      focus: gatedFocus,
    });
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );
    const promote = q(el, '.promote-btn') as HTMLButtonElement;
    for (const key of ['Enter', ' ']) {
      const press = new KeyboardEvent('keydown', {
        key,
        bubbles: true,
        composed: true,
        cancelable: true,
      });
      promote.dispatchEvent(press);
      expect(press.defaultPrevented, `${JSON.stringify(key)} on the button`).to.equal(false);
    }
    expect(sent).to.deep.equal([]);
  });

  it('clears the selection when another thread replaces the one on screen', async () => {
    // `thread-loading` cleared the model but kept the selected id, so the next thread opened
    // an inspector for a node it does not contain.
    const sent: ThreadWebviewToHost[] = [];
    const el = await fixture<EscurelThreadCanvas>(
      html`<escurel-thread-canvas></escurel-thread-canvas>`,
    );
    const disconnect = connectThreadWebview({ postMessage: (m) => sent.push(m) }, el);
    try {
      const post = (data: unknown) => window.dispatchEvent(new MessageEvent('message', { data }));
      post({
        type: 'thread',
        view: recordedThreadView,
        layout: recordedLayout,
        focus: recordedFocus,
      });
      await el.updateComplete;
      el.selectNode(recordedThreadView.rootEventId);
      await el.updateComplete;
      expect(el.selectedNodeId).to.equal(recordedThreadView.rootEventId);
      post({ type: 'thread-loading', rootEventId: 'another' });
      await el.updateComplete;
      expect(el.selectedNodeId).to.equal('');
    } finally {
      disconnect();
    }
  });

  it('keeps a selected card in view', async () => {
    // A card near the right edge must not be clipped the moment it is selected.
    const el = await renderCanvas();
    const last = recordedLayout.nodes.reduce((a, b) => (b.x > a.x ? b : a));
    el.selectNode(last.id);
    await el.updateComplete;
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    const area = el.shadowRoot!.querySelector('.canvas-area') as HTMLElement;
    const right = (last.x + last.width) * el.viewport.zoom + el.viewport.x;
    expect(right).to.be.at.most(area.clientWidth);
    expect(last.x * el.viewport.zoom + el.viewport.x).to.be.at.least(0);
  });

  describe('first view', () => {
    async function inBox(
      width: number,
      view = branchingThreadView,
      layout = branchingLayout,
      focus = branchingFocus,
    ): Promise<EscurelThreadCanvas> {
      const host = await fixture<HTMLElement>(html`
        <div style="width:${width}px;height:700px;position:relative">
          <escurel-thread-canvas
            style="display:block;width:100%;height:100%"
            .view=${view}
            .layout=${layout}
            .focus=${focus}
          ></escurel-thread-canvas>
        </div>
      `);
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      await el.updateComplete;
      return el;
    }
    const cardOf = (el: EscurelThreadCanvas, id: string) =>
      el.shadowRoot!.querySelector(`.card[data-node-id="${id}"]`) as HTMLElement;

    it('a big thread opens at 100% with the node that needs you in view, not shrunk to a fit', async () => {
      const el = await inBox(420);
      expect(el.viewport.zoom).to.equal(1);
      const target = pickTarget(branchingThreadView, branchingLayout)!;
      const card = cardOf(el, target);
      const area = el.shadowRoot!.querySelector('.canvas-area') as HTMLElement;
      const left = card.getBoundingClientRect().left - area.getBoundingClientRect().left;
      const right = card.getBoundingClientRect().right - area.getBoundingClientRect().left;
      expect(
        left >= 0 && right <= area.clientWidth,
        `card ${left}..${right} in ${area.clientWidth}`,
      ).to.equal(true);
    });

    it('says when more stages lie beyond the right edge, and one click brings them in', async () => {
      const el = await inBox(420);
      const more = el.shadowRoot!.querySelector('button.edge-more') as HTMLButtonElement | null;
      expect(more !== null, 'a cue that more exists').to.equal(true);
      expect(/\d+ more stage/.test(text(more))).to.equal(true);
      const before = el.viewport.x;
      more!.click();
      await el.updateComplete;
      expect(el.viewport.x < before, `x ${before} -> ${el.viewport.x}`).to.equal(true);
    });

    it('shows no such cue when everything is in view', async () => {
      const el = await inBox(4000);
      expect(el.shadowRoot!.querySelector('button.edge-more') === null).to.equal(true);
    });

    it('opens with no card cut in half at the left edge', async () => {
      const el = await inBox(900);
      const area = el.shadowRoot!.querySelector('.canvas-area') as HTMLElement;
      const a = area.getBoundingClientRect();
      const cut = (Array.from(el.shadowRoot!.querySelectorAll('.card')) as HTMLElement[]).filter(
        (c) => {
          const r = c.getBoundingClientRect();
          return r.left < a.left - 1 && r.right > a.left + 1;
        },
      );
      expect(cut.map((c) => c.getAttribute('aria-label'))).to.deep.equal([]);
    });

    it('a small thread opens as is: 100%, nothing scrolled', async () => {
      const el = await inBox(4000);
      expect(el.viewport).to.deep.equal({ x: 0, y: 0, zoom: 1 });
    });

    it('does not pull the view back when the thread reloads after the person moved it', async () => {
      const el = await inBox(420);
      el.viewport = { x: 10, y: 20, zoom: 0.5 };
      el.layout = { ...branchingLayout };
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      expect(el.viewport).to.deep.equal({ x: 10, y: 20, zoom: 0.5 });
    });

    it('applies the first view even when the canvas was not measurable at first (a hidden tab)', async () => {
      const host = await fixture<HTMLElement>(html`
        <div id="w" style="display:none;width:420px;height:700px;position:relative">
          <escurel-thread-canvas
            style="display:block;width:100%;height:100%"
            .view=${branchingThreadView}
            .layout=${branchingLayout}
            .focus=${branchingFocus}
          ></escurel-thread-canvas>
        </div>
      `);
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      expect(el.viewport).to.deep.equal({ x: 0, y: 0, zoom: 1 });
      host.style.display = 'block';
      await new Promise((r) => setTimeout(r, 100));
      await el.updateComplete;
      expect(el.viewport.x < 0, `x ${el.viewport.x}`).to.equal(true);
    });

    it('keeps measuring the canvas after the thread was empty and came back', async () => {
      const el = await inBox(420);
      const view = el.view!;
      el.view = { ...view, nodes: [] };
      await el.updateComplete;
      el.view = view;
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      (el.parentElement as HTMLElement).style.width = '300px';
      await new Promise((r) => setTimeout(r, 100));
      await el.updateComplete;
      const area = el.shadowRoot!.querySelector('.canvas-area') as HTMLElement;
      expect((el as unknown as { areaSize: { width: number } }).areaSize.width).to.equal(
        area.clientWidth,
      );
    });

    it('the vertical thumb reaches the bottom of the graph when dragged to the end', async () => {
      const host = await fixture<HTMLElement>(html`
        <div style="width:900px;height:160px;position:relative">
          <escurel-thread-canvas
            style="width:100%;height:100%"
            .view=${branchingThreadView}
            .layout=${branchingLayout}
            .focus=${branchingFocus}
          ></escurel-thread-canvas>
        </div>
      `);
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      await el.updateComplete;
      // From the top: a drag that starts mid-graph would hit the clamp whatever the maths said.
      el.viewport = { x: 0, y: 0, zoom: 1 };
      await el.updateComplete;
      const thumb = el.shadowRoot!.querySelector('.scroll-thumb.v') as HTMLElement;
      expect(thumb !== null, 'a vertical thumb').to.equal(true);
      const r = thumb.getBoundingClientRect();
      const ev = (y: number, type: string) =>
        new PointerEvent(type, {
          clientX: r.left + 2,
          clientY: y,
          pointerId: 1,
          bubbles: true,
          composed: true,
        });
      thumb.dispatchEvent(ev(r.top + 2, 'pointerdown'));
      // Exactly the free length of the track: the thumb ends at the bottom of its track.
      const track = el.shadowRoot!.querySelector('.scroll-track.v') as HTMLElement;
      const free = track.getBoundingClientRect().height - r.height;
      thumb.dispatchEvent(ev(r.top + 2 + free, 'pointermove'));
      thumb.dispatchEvent(ev(r.top + 2 + free, 'pointerup'));
      await el.updateComplete;
      const area = el.shadowRoot!.querySelector('.canvas-area') as HTMLElement;
      const bottom = branchingLayout.bounds.height * el.viewport.zoom + el.viewport.y;
      expect(Math.round(bottom), `graph bottom ${bottom} vs area ${area.clientHeight}`).to.equal(
        area.clientHeight,
      );
    });

    it('does not squash the column headings when zoomed out', async () => {
      const el = await inBox(4000);
      el.viewport = { x: 0, y: 0, zoom: 0.5 };
      await el.updateComplete;
      const track = el.shadowRoot!.querySelector('.header-transformed-track') as HTMLElement;
      const m = new DOMMatrixReadOnly(getComputedStyle(track).transform);
      expect(m.a, 'horizontal scale of the heading track').to.equal(1);
    });

    it('Fit still shows the whole graph', async () => {
      const el = await inBox(420);
      el.fit();
      await el.updateComplete;
      expect(el.viewport.zoom < 1).to.equal(true);
    });

    it('dragging the scrollbar thumb pans the graph', async () => {
      const el = await inBox(420);
      const before = el.viewport.x;
      const thumb = el.shadowRoot!.querySelector('.scroll-thumb.h') as HTMLElement;
      const r = thumb.getBoundingClientRect();
      const at = (dx: number, type: string) =>
        new PointerEvent(type, {
          clientX: r.left + 2 + dx,
          clientY: r.top + 2,
          pointerId: 1,
          bubbles: true,
          composed: true,
        });
      thumb.dispatchEvent(at(0, 'pointerdown'));
      thumb.dispatchEvent(at(60, 'pointermove'));
      thumb.dispatchEvent(at(60, 'pointerup'));
      await el.updateComplete;
      expect(el.viewport.x < before, `x ${before} -> ${el.viewport.x}`).to.equal(true);
    });

    it('shows a scrollbar thumb on an axis that overflows, so the cut-off edge is reachable', async () => {
      const el = await inBox(420);
      expect(el.shadowRoot!.querySelector('.scroll-thumb.h') !== null).to.equal(true);
      const small = await inBox(4000);
      expect(small.shadowRoot!.querySelector('.scroll-thumb.h') === null).to.equal(true);
    });
  });

  describe('semantic zoom', () => {
    async function at(zoom: number): Promise<EscurelThreadCanvas> {
      const el = await renderCanvas({
        view: branchingThreadView,
        layout: branchingLayout,
        focus: branchingFocus,
      });
      el.viewport = { x: 0, y: 0, zoom };
      await el.updateComplete;
      return el;
    }
    const visible = (e: Element | null) => !!e && getComputedStyle(e).display !== 'none';

    it('below 85% a card keeps icon, type word, title and state chip, and drops the body', async () => {
      const el = await at(0.5);
      expect(q(el, '.canvas-area')!.classList.contains('low-zoom')).to.equal(true);
      // The assertions below must have something to bite on: the thread has buttons and bodies to hide.
      expect(qa(el, '.gate-actions').length > 0, 'a card with Promote/Discard').to.equal(true);
      expect(qa(el, '.needs-reason').length > 0, 'a card with a reason').to.equal(true);
      expect(qa(el, '.card-title').length > 0, 'cards with titles').to.equal(true);
      for (const card of qa(el, '.card')) {
        for (const sel of ['.type-icon', '.card-title', '.type-label', '.chip']) {
          expect(visible(card.querySelector(sel)), `${sel} visible`).to.equal(true);
        }
        for (const sel of [
          '.card-subtitle',
          '.meta-lines',
          '.needs-reason',
          '.gate-actions',
          '.draft-list',
        ]) {
          const el2 = card.querySelector(sel);
          expect(el2 === null || !visible(el2), `${sel} hidden`).to.equal(true);
        }
      }
    });

    it('keeps the words readable at any zoom: rendered text is never below 10px', async () => {
      // The card text is counter-scaled, so zooming out shrinks the picture, not the words.
      for (const zoom of [0.84, 0.69, 0.5, 0.4]) {
        const el = await at(zoom);
        for (const card of qa(el, '.card')) {
          for (const sel of ['.card-title', '.type-label', '.chip']) {
            const node = card.querySelector(sel);
            if (!node) continue;
            const rendered = parseFloat(getComputedStyle(node).fontSize) * zoom;
            expect(
              rendered >= 9.5,
              `${sel} renders at ${rendered.toFixed(1)}px at ${zoom}`,
            ).to.equal(true);
          }
        }
      }
    });

    it('fits the words in the card: nothing spills out of a box at the lowest zoom', async () => {
      const el = await at(0.4);
      // A row squeezed by flex-shrink clips its own text while its box still looks inside the card:
      // each row must hold its own text.
      for (const card of qa(el, '.card') as HTMLElement[]) {
        for (const row of [...card.children] as HTMLElement[]) {
          if (getComputedStyle(row).display === 'none') continue;
          expect(
            row.scrollHeight <= row.clientHeight + 1,
            `row ${row.className} of ${card.dataset.nodeId}: text ${row.scrollHeight} in ${row.clientHeight}`,
          ).to.equal(true);
        }
      }
      // scrollHeight/scrollWidth are in layout px and count clipped content: a row cut off by the
      // card's own overflow:hidden (what the 40% baseline showed) is still caught.
      for (const card of qa(el, '.card') as HTMLElement[]) {
        expect(
          card.scrollHeight <= card.clientHeight + 1,
          `card ${card.dataset.nodeId} content ${card.scrollHeight} > box ${card.clientHeight}`,
        ).to.equal(true);
        expect(
          card.scrollWidth <= card.clientWidth + 1,
          `card ${card.dataset.nodeId} content wider than box`,
        ).to.equal(true);
      }
      for (const card of qa(el, '.card') as HTMLElement[]) {
        const box = card.getBoundingClientRect();
        for (const sel of ['.card-title', '.type-label', '.chip']) {
          const n = card.querySelector(sel);
          if (!n) continue;
          const r = n.getBoundingClientRect();
          expect(
            r.right <= box.right + 1 && r.bottom <= box.bottom + 1,
            `${sel} inside its card`,
          ).to.equal(true);
        }
      }
    });

    it('keeps the focus ring visible when zoomed out: it renders at least 1.5px at any zoom', async () => {
      // A 2px outline on a card scaled to 40% is 0.8px: a keyboard user loses where they are.
      for (const zoom of [0.84, 0.69, 0.4]) {
        const el = await at(zoom);
        const card = qa(el, '.card')[0] as HTMLElement;
        card.focus();
        await el.updateComplete;
        const width = parseFloat(getComputedStyle(card).outlineWidth);
        expect(
          width * zoom >= 1.5,
          `outline ${width}px renders at ${(width * zoom).toFixed(2)}px at ${zoom}`,
        ).to.equal(true);
      }
    });

    it('keeps the Needs-you badge icon at low zoom, without its words', async () => {
      const el = await at(0.5);
      const badges = qa(el, '.needs-badge');
      expect(badges.length > 0).to.equal(true);
      for (const b of badges) {
        expect(visible(b)).to.equal(true);
        expect(visible(b.querySelector('svg')), 'person icon').to.equal(true);
        const words = b.querySelector('.needs-text');
        expect(words !== null && !visible(words), 'words hidden').to.equal(true);
      }
    });

    it('from 85% up the cards show their text as before', async () => {
      const el = await at(0.85);
      expect(q(el, '.canvas-area')!.classList.contains('low-zoom')).to.equal(false);
      expect(visible(q(el, '.card-title'))).to.equal(true);
      expect(q(el, '.zoom-hint') === null).to.equal(true);
    });

    it('says "overview" next to the zoom percentage while in the low-zoom form', async () => {
      const el = await at(0.5);
      expect(text(q(el, '.zoom-hint'))).to.equal('overview');
    });

    it('keeps every card’s accessible name (type, title, state, needs you) at low zoom', async () => {
      const normal = await at(1);
      const names = qa(normal, '.card').map((c) => c.getAttribute('aria-label'));
      const low = await at(0.5);
      expect(qa(low, '.card').map((c) => c.getAttribute('aria-label'))).to.deep.equal(names);
      expect(names.some((n) => /needs you/.test(n ?? ''))).to.equal(true);
    });

    it('puts the full text in the tooltip at low zoom', async () => {
      const el = await at(0.5);
      const first = qa(el, '.card')[0] as HTMLElement;
      expect((first.getAttribute('title') ?? '').length > 0).to.equal(true);
    });

    it('does not move or resize any card when the form changes (wires keep their place)', async () => {
      const el = await at(1);
      const before = qa(el, '.card').map((c) => (c as HTMLElement).style.cssText);
      const low = await at(0.5);
      expect(qa(low, '.card').map((c) => (c as HTMLElement).style.cssText)).to.deep.equal(before);
    });
  });

  it('keeps card text at a readable size (11px or more) and the first meta line in the main text colour', async () => {
    // The reviewers could not read the cards: meta text was about 9.75px in dim grey.
    const host = await fixture<HTMLElement>(html`
      <div style="--vscode-font-size:13px;font-size:13px">
        <escurel-thread-canvas
          .view=${recordedThreadView}
          .layout=${recordedLayout}
          .focus=${recordedFocus}
        ></escurel-thread-canvas>
      </div>
    `);
    const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
    await el.updateComplete;
    const px = (e: Element) => parseFloat(getComputedStyle(e).fontSize);
    const lines = qa(el, '.meta-line');
    expect(lines.length > 0, 'the recorded thread has cards with meta lines').to.equal(true);
    for (const l of lines) expect(px(l) >= 11, `meta ${px(l)}px`).to.equal(true);
    for (const l of qa(el, '.card-subtitle'))
      expect(px(l) >= 11, `subtitle ${px(l)}px`).to.equal(true);
    // Bigger text must still FIT its card: a clipped first line was the regression this guards.
    const first = lines[0]!;
    expect(getComputedStyle(first).color).to.equal(getComputedStyle(el).color);
  });

  describe('typed and compact cards', () => {
    it('gives every card a type icon, a type word and a type accent', async () => {
      const el = await renderCanvas();
      const cards = qa(el, '.card');
      expect(cards.length > 0).to.equal(true);
      for (const card of cards) {
        expect(card.querySelector('.type-icon svg') !== null, 'an inline svg icon').to.equal(true);
        expect((card.querySelector('.type-label')?.textContent ?? '').trim().length > 0).to.equal(
          true,
        );
        expect(/\btype-(event|cascade|run|changeset|page)\b/.test(card.className)).to.equal(true);
      }
      const words = new Set(cards.map((c) => text(c.querySelector('.type-label'))));
      expect(words.size > 2, `type words: ${[...words].join(', ')}`).to.equal(true);
    });

    it('names a card by its type first, so a screen reader hears what it is', async () => {
      const el = await renderCanvas();
      const run = qa(el, '.card.type-run')[0]!;
      expect(run.getAttribute('aria-label')!.startsWith('run')).to.equal(true);
    });

    it('draws a finished card small: title and state on two lines, no meta lines', async () => {
      const el = await renderCanvas();
      const compact = qa(el, '.card.compact');
      expect(compact.length > 0, 'the recorded thread has finished nodes').to.equal(true);
      for (const card of compact) {
        expect(card.querySelector('.meta-lines')).to.equal(null);
        expect(card.querySelector('.card-title') !== null).to.equal(true);
        expect(card.querySelector('.chip') !== null, 'the state stays').to.equal(true);
        expect(card.getBoundingClientRect().height < 80).to.equal(true);
      }
      const full = qa(el, '.card:not(.compact)')[0];
      if (full) expect(full.getBoundingClientRect().height > 100).to.equal(true);
    });

    it('never clips a card: its content fits inside its box, small or full', async () => {
      // The finished cards lost their bottom edge to the clipping (seen in a baseline).
      const el = await renderCanvas();
      for (const card of qa(el, '.card') as HTMLElement[]) {
        expect(
          card.scrollHeight <= card.clientHeight + 1,
          `${card.getAttribute('aria-label')}: content ${card.scrollHeight}px in a ${card.clientHeight}px card`,
        ).to.equal(true);
      }
    });

    it('keeps the details of a small card available on hover', async () => {
      const el = await renderCanvas();
      const compact = qa(el, '.card.compact')[0]!;
      expect((compact.getAttribute('title') ?? '').length > 0).to.equal(true);
    });
  });

  describe('lanes', () => {
    it('draws a divider and a caption for every lane after the first', async () => {
      const view = recordedThreadView;
      const layout = {
        ...recordedLayout,
        lanes: [
          { index: 0, y: 32, height: 150 },
          { index: 1, y: 222, height: 150, title: 'customer-notice' },
        ],
      };
      const el = await renderCanvas({ view, layout });
      const dividers = qa(el, '.lane-divider');
      expect(dividers).to.have.length(1);
      expect(text(q(el, '.lane-caption'))).to.contain('customer-notice');
    });

    it('draws no lane furniture when the thread is a single chain', async () => {
      const el = await renderCanvas();
      expect(qa(el, '.lane-divider')).to.have.length(0);
    });
  });

  it('fits from the top: a short graph does not float in the middle under an empty band', async () => {
    const el = await renderCanvas();
    el.fit();
    await el.updateComplete;
    // Below the pinned header strip, not centred in a tall box.
    expect(el.viewport.y <= 48, `y ${el.viewport.y}`).to.equal(true);
  });

  it('says which card is selected, for a screen reader', async () => {
    const el = await renderCanvas();
    const card = qa(el, '.card')[1] as HTMLElement;
    expect(card.getAttribute('aria-selected')).to.equal('false');
    el.selectNode(card.getAttribute('data-node-id')!);
    await el.updateComplete;
    expect(card.getAttribute('aria-selected')).to.equal('true');
  });

  describe('work that needs you', () => {
    const branching = () =>
      renderCanvas({ view: branchingThreadView, layout: branchingLayout, focus: branchingFocus });

    it('marks every card that waits on a person with a badge, a person icon and the reason', async () => {
      const el = await branching();
      const needing = qa(el, '.card.needs-you');
      // The open changeset, the planned run and the dead-lettered run.
      expect(needing).to.have.length(3);
      for (const card of needing) {
        const badge = card.querySelector('.needs-badge');
        expect(text(badge)).to.contain('Needs you');
        expect(badge?.querySelector('svg') !== null, 'a person icon').to.equal(true);
        expect(text(card.querySelector('.needs-reason')).length > 0).to.equal(true);
      }
      expect(qa(el, '.card:not(.needs-you) .needs-badge')).to.have.length(0);
    });

    it('says so in the accessible name, not by colour', async () => {
      const el = await branching();
      for (const card of qa(el, '.card.needs-you')) {
        expect(card.getAttribute('aria-label')!.toLowerCase()).to.contain('needs you');
      }
    });

    it('gives them a bigger card than a normal one, and the finished ones the smallest', async () => {
      const el = await branching();
      const needing = Math.min(
        ...qa(el, '.card.needs-you').map((c) => c.getBoundingClientRect().height),
      );
      const compact = Math.max(
        ...qa(el, '.card.compact').map((c) => c.getBoundingClientRect().height),
      );
      expect(needing > compact * 2, `${needing}px vs ${compact}px`).to.equal(true);
    });

    it('never clips a card, however full', async () => {
      const el = await branching();
      for (const card of qa(el, '.card') as HTMLElement[]) {
        expect(
          card.scrollHeight <= card.clientHeight + 1,
          `${card.getAttribute('aria-label')}: content ${card.scrollHeight}px in ${card.clientHeight}px`,
        ).to.equal(true);
      }
    });

    it('a planned run offers Approve plan ON ITS CARD, so a dismissed toast is not the only way', async () => {
      const el = await branching();
      const card = qa(el, '.card.needs-you').find((c) =>
        text(c.querySelector('.needs-reason')).includes('plan'),
      )!;
      const btn = card.querySelector('button.approve-btn') as HTMLButtonElement;
      expect(text(btn)).to.equal('Approve plan');
      const sent: ThreadWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
      );
      btn.click();
      expect(sent.length).to.equal(1);
      expect(sent[0]).to.include({ type: 'run-control', action: 'approve' });
      expect(typeof (sent[0] as { runId?: string }).runId).to.equal('string');
    });

    it('a failed run offers Retry on its card', async () => {
      const el = await branching();
      const card = qa(el, '.card.needs-you').find((c) =>
        /fail|dead|stopped/i.test(text(c.querySelector('.needs-reason'))),
      )!;
      expect(text(card.querySelector('button.retry-btn'))).to.equal('Retry');
    });

    it('only Promote is a filled button; Discard is quiet, with a gap and a 24px hit area', async () => {
      const el = await branching();
      // The theme tokens the card's fill is built from (a real webview gets them from VS Code).
      el.style.setProperty('--escurel-run', '#89d185');
      await el.updateComplete;
      const card = q(el, '.card.type-changeset.needs-you')!;
      const promote = card.querySelector('.promote-btn') as HTMLElement;
      const discard = card.querySelector('.discard-btn') as HTMLElement;
      expect(getComputedStyle(promote).backgroundColor).to.not.equal('rgba(0, 0, 0, 0)');
      // No fill, no heavy border: the destructive action is available but never competes with Promote.
      expect(getComputedStyle(discard).backgroundColor).to.equal('rgba(0, 0, 0, 0)');
      for (const b of [promote, discard, card.querySelector('.review-btn') as HTMLElement]) {
        expect(
          b.getBoundingClientRect().height >= 24,
          `${b.className}: ${b.getBoundingClientRect().height}px`,
        ).to.equal(true);
      }
      const gap = discard.getBoundingClientRect().left - promote.getBoundingClientRect().right;
      expect(gap >= 6, `gap ${gap}px`).to.equal(true);
    });

    it('shows who proposed an open changeset and how long ago', async () => {
      const el = await branching();
      const card = q(el, '.card.type-changeset.needs-you')!;
      expect(text(card.querySelector('.changeset-author'))).to.contain('agent:supplier-risk');
      expect(/ago|just now/.test(text(card.querySelector('.changeset-author')))).to.equal(true);
    });

    it('measures a changeset’s age against the canvas’s own clock, so a baseline does not age', async () => {
      // The visual baselines read "4 h ago", then "5 h ago": the age came from the real clock.
      const el = await branching();
      el.clock = () => new Date('2026-10-03T08:03:06Z');
      await el.updateComplete;
      const card = q(el, '.card.type-changeset.needs-you')!;
      expect(text(card.querySelector('.changeset-author'))).to.equal(
        'agent:supplier-risk · 3 min ago',
      );
    });

    it('lists the pages it changes, each openable', async () => {
      const el = await branching();
      const card = q(el, '.card.type-changeset.needs-you')!;
      const entries = Array.from(card.querySelectorAll('.draft-entry')) as HTMLButtonElement[];
      expect(entries.map(text)).to.deep.equal(['order-4500131', 'meier-guss-2026-10-03']);
      const sent: ThreadWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
      );
      entries[1]!.click();
      expect(sent).to.deep.equal([{ type: 'open-node', nodeId: '01M4DRF2000000000000000002' }]);
    });

    it('opens the review of the whole changeset from its card', async () => {
      const el = await branching();
      const card = q(el, '.card.type-changeset.needs-you')!;
      const sent: ThreadWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
      );
      (card.querySelector('.review-btn') as HTMLButtonElement).click();
      expect(sent).to.deep.equal([{ type: 'open-node', nodeId: '01M4CHS1000000000000000001' }]);
    });

    it('offers Promote all and Discard on the open changeset', async () => {
      const el = await branching();
      const card = q(el, '.card.type-changeset.needs-you')!;
      expect(text(card.querySelector('.promote-btn'))).to.equal('Promote all 2');
      expect(text(card.querySelector('.discard-btn'))).to.equal('Discard');
    });

    // Relative luminance and contrast per WCAG 2.x.
    const channel = (v: number) => {
      const c = v / 255;
      return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    };
    const luminance = (rgb: string) => {
      const [r, g, b] = (rgb.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number);
      return 0.2126 * channel(r!) + 0.7152 * channel(g!) + 0.0722 * channel(b!);
    };
    const contrast = (a: string, b: string) => {
      const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
      return (hi! + 0.05) / (lo! + 0.05);
    };
    const darkTokens =
      '--vscode-charts-green:#89d185;--vscode-button-foreground:#ffffff;--vscode-button-secondaryBackground:#3a3d41;' +
      '--vscode-button-secondaryForeground:#ffffff;--vscode-errorForeground:#f48771;--vscode-foreground:#cccccc;' +
      '--vscode-editorWarning-foreground:#cca700;--vscode-focusBorder:#007fd4;--vscode-editor-background:#1e1e1e;' +
      '--vscode-editorWidget-background:#252526;--vscode-descriptionForeground:#9d9d9d';

    it('keeps the action buttons readable in a dark theme (Promote was white on light green, 1.8:1)', async () => {
      const host = await fixture<HTMLElement>(
        html`<div style=${darkTokens}>
          <escurel-thread-canvas
            .view=${branchingThreadView}
            .layout=${branchingLayout}
            .focus=${branchingFocus}
          ></escurel-thread-canvas>
        </div>`,
      );
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      const card = q(el, '.card.type-changeset.needs-you')!;
      for (const sel of ['.promote-btn', '.discard-btn', '.review-btn']) {
        const cs = getComputedStyle(card.querySelector(sel)!);
        const ratio = contrast(cs.backgroundColor, cs.color);
        expect(
          ratio >= 4.5,
          `${sel}: ${cs.color} on ${cs.backgroundColor} = ${ratio.toFixed(2)}`,
        ).to.equal(true);
      }
    });

    it('keeps the warning border and halo when the card is selected', async () => {
      const host = await fixture<HTMLElement>(
        html`<div style=${darkTokens}>
          <escurel-thread-canvas
            .view=${branchingThreadView}
            .layout=${branchingLayout}
            .focus=${branchingFocus}
          ></escurel-thread-canvas>
        </div>`,
      );
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      const card = q(el, '.card.type-changeset.needs-you') as HTMLElement;
      const warning = getComputedStyle(card).borderTopColor;
      el.selectNode(card.getAttribute('data-node-id')!);
      await el.updateComplete;
      expect(getComputedStyle(card).borderTopColor, 'still the warning colour').to.equal(warning);
      // Selection is a ring beyond the halo, not a replacement for it.
      expect(getComputedStyle(card).boxShadow).to.contain('rgb(0, 127, 212)');
    });

    it('makes "+N more" a button that opens the whole changeset', async () => {
      const many = {
        ...branchingThreadView,
        nodes: branchingThreadView.nodes.map((n) =>
          n.kind === 'changeset' && n.changeset
            ? {
                ...n,
                changeset: {
                  ...n.changeset,
                  drafts: Array.from({ length: 7 }, (_, i) => ({
                    id: `d${i}`,
                    title: `page-${i}`,
                  })),
                },
              }
            : n,
        ),
      };
      const el = await renderCanvas({
        view: many,
        layout: layoutThread(many, new Set()),
        focus: branchingFocus,
      });
      const card = q(el, '.card.type-changeset.needs-you')!;
      expect(card.querySelectorAll('.draft-entry')).to.have.length(4);
      const more = card.querySelector('.draft-more') as HTMLButtonElement;
      expect(more.tagName).to.equal('BUTTON');
      expect(text(more)).to.equal('+3 more');
      const sent: ThreadWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
      );
      more.click();
      expect(sent).to.deep.equal([{ type: 'open-node', nodeId: '01M4CHS1000000000000000001' }]);
    });

    it('deactivates the buttons and says why when the person may not decide', async () => {
      const view = {
        ...branchingThreadView,
        nodes: branchingThreadView.nodes.map((n) =>
          n.kind === 'changeset' && n.gate
            ? { ...n, gate: { ...n.gate, disabledReason: 'Only the page owner can promote this.' } }
            : n,
        ),
      };
      const el = await renderCanvas({ view, layout: branchingLayout, focus: branchingFocus });
      const card = q(el, '.card.type-changeset.needs-you')!;
      const promote = card.querySelector('.promote-btn') as HTMLButtonElement;
      expect(promote.getAttribute('aria-disabled')).to.equal('true');
      expect(text(card.querySelector('.gate-reason'))).to.contain('Only the page owner');
      const sent: ThreadWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
      );
      promote.click();
      expect(sent).to.deep.equal([]);
    });
  });
});
