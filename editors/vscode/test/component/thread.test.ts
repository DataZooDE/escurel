import { expect, fixture, html } from '@open-wc/testing';
import type { ThreadWebviewToHost } from '../../src/shared/protocol';
import { connectThreadWebview } from '../../webview/thread/main';
import type { EscurelThreadCanvas } from '../../webview/thread/thread-canvas';
import '../../webview/thread/thread-canvas';
import {
  gatedFocus,
  gatedLayout,
  gatedThreadView,
  recordedDetails,
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
      .details=${props.details ?? recordedDetails}
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

    // Each visible card is placed in the DOM with treeitem role and column-derived aria-level.
    for (const node of visibleNodes) {
      const card = q(el, `.card[data-node-id="${node.id}"]`);
      expect(card).to.exist;
      expect(card?.getAttribute('role')).to.equal('treeitem');
      expect(card?.getAttribute('aria-level')).to.equal(String(node.column + 1));
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

    // Inspector is rendered for selected node.
    const inspector = q(el, 'escurel-thread-inspector') as { detail?: { title?: string } } | null;
    expect(inspector).to.exist;
    expect(inspector?.detail?.title).to.equal('signal run');
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
            details: recordedDetails,
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
        details: recordedDetails,
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

  it('keeps a selected card in view once the inspector has taken its share of the width', async () => {
    // Selecting opens the inspector, which takes 320px from the canvas; the reveal was
    // computed against the canvas as it was BEFORE that, so a card near the right edge could
    // be clipped the moment it was selected.
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
    async function inBox(width: number): Promise<EscurelThreadCanvas> {
      const host = await fixture<HTMLElement>(html`
        <div style="width:${width}px;height:700px;position:relative">
          <escurel-thread-canvas
            style="display:block;width:100%;height:100%"
            .view=${recordedThreadView}
            .layout=${recordedLayout}
            .focus=${recordedFocus}
            .details=${recordedDetails}
          ></escurel-thread-canvas>
        </div>
      `);
      const el = host.querySelector('escurel-thread-canvas') as EscurelThreadCanvas;
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      await el.updateComplete;
      return el;
    }

    it('fits a thread that is wider than the window, instead of cropping it at 100%', async () => {
      // Seen in the live window: the third column was cut off with no cue that more existed.
      const el = await inBox(420);
      expect(el.viewport.zoom < 1, `zoom ${el.viewport.zoom}`).to.equal(true);
    });

    it('never opens smaller than a readable size: a big thread scrolls instead of shrinking to 30%', async () => {
      // A nine-node thread fitted at 48% made every card unreadable.
      const el = await inBox(300);
      expect(el.viewport.zoom >= 0.7, `zoom ${el.viewport.zoom}`).to.equal(true);
      expect(el.viewport.zoom < 1).to.equal(true);
    });

    it('leaves a thread that fits at 100%', async () => {
      const el = await inBox(4000);
      expect(el.viewport.zoom).to.equal(1);
    });

    it('does not pull the view back when the thread reloads after the person moved it', async () => {
      const el = await inBox(420);
      el.viewport = { x: 10, y: 20, zoom: 0.5 };
      el.layout = { ...recordedLayout };
      await el.updateComplete;
      await new Promise((r) => requestAnimationFrame(() => r(undefined)));
      expect(el.viewport).to.deep.equal({ x: 10, y: 20, zoom: 0.5 });
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
          .details=${recordedDetails}
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
});
