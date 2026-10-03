import { expect, fixture, html } from '@open-wc/testing';
import type { DetailsWebviewToHost, ShownDetails } from '../../src/shared/protocol';
import { connectDetailsWebview } from '../../webview/details/main';
import type { EscurelDetails } from '../../webview/details/details';
import '../../webview/details/details';

const runDetail: ShownDetails = {
  rootEventId: 'root-A',
  nodeId: 'run-1',
  detail: {
    title: 'Echo run',
    rows: [{ k: 'state', v: 'failed' }],
    sideTitle: '',
    side: [],
    actions: {
      controls: [
        { action: 'retry', label: 'Retry', enabled: true },
        {
          action: 'requeue',
          label: 'Requeue',
          enabled: false,
          disabledReason: 'Only an admin can requeue.',
        },
      ],
      skill: 'supplier-risk',
    },
  },
};

async function render(): Promise<EscurelDetails> {
  const el = await fixture<EscurelDetails>(html`<escurel-details></escurel-details>`);
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const text = (n: Element | null | undefined) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();
const inner = (el: Element) => q(el, 'escurel-thread-inspector')!;

describe('<escurel-details>', () => {
  it('says what to do when nothing is selected', async () => {
    const el = await render();
    expect(text(q(el, '.empty'))).to.equal('Select a node in a thread to see its details.');
    expect(q(el, 'escurel-thread-inspector')).to.equal(null);
  });

  it('shows the selected node through the inspector', async () => {
    const el = await render();
    el.shown = runDetail;
    await el.updateComplete;
    expect(q(el, '.empty')).to.equal(null);
    await (inner(el) as unknown as { updateComplete: Promise<boolean> }).updateComplete;
    expect(text(inner(el).shadowRoot!.querySelector('h2'))).to.equal('Echo run');
  });

  it('wraps an inspector button with the thread it was shown for', async () => {
    const el = await render();
    el.shown = runDetail;
    await el.updateComplete;
    await (inner(el) as unknown as { updateComplete: Promise<boolean> }).updateComplete;
    const sent: DetailsWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<DetailsWebviewToHost>).detail),
    );
    (inner(el).shadowRoot!.querySelector('.control-button') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      {
        type: 'details-action',
        rootEventId: 'root-A',
        message: { type: 'run-control', action: 'retry', runId: 'run-1' },
      },
    ]);
  });

  it('Escape gives the focus back to the thread it belongs to', async () => {
    const el = await render();
    el.shown = runDetail;
    await el.updateComplete;
    const sent: DetailsWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<DetailsWebviewToHost>).detail),
    );
    el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(sent).to.deep.equal([{ type: 'focus-canvas', rootEventId: 'root-A' }]);
  });
});

describe('connectDetailsWebview', () => {
  it('announces itself, shows what the host sends and clears on details-empty', async () => {
    const el = await render();
    const posted: DetailsWebviewToHost[] = [];
    const stop = connectDetailsWebview({ postMessage: (m) => posted.push(m) }, el);
    expect(posted).to.deep.equal([{ type: 'ready' }]);
    window.dispatchEvent(new MessageEvent('message', { data: { type: 'details', ...runDetail } }));
    await el.updateComplete;
    expect(el.shown?.nodeId).to.equal('run-1');
    window.dispatchEvent(new MessageEvent('message', { data: { type: 'details-empty' } }));
    await el.updateComplete;
    expect(el.shown).to.equal(undefined);
    stop();
  });

  it('forwards the element’s messages to the host', async () => {
    const el = await render();
    const posted: DetailsWebviewToHost[] = [];
    const stop = connectDetailsWebview({ postMessage: (m) => posted.push(m) }, el);
    el.shown = runDetail;
    await el.updateComplete;
    el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(posted.at(-1)).to.deep.equal({ type: 'focus-canvas', rootEventId: 'root-A' });
    stop();
  });
});
