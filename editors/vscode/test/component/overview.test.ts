import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/overview/main';
import type { EscurelOverview } from '../../webview/overview/overview';
import type { OverviewWebviewToHost } from '../../src/shared/protocol';
import { morning, quiet } from './overview-fixtures';

const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => Array.from(el.shadowRoot!.querySelectorAll(sel));
const text = (n: Element | null | undefined) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

async function render(view = morning): Promise<EscurelOverview> {
  const el = await fixture<EscurelOverview>(
    html`<escurel-overview .view=${view}></escurel-overview>`,
  );
  await el.updateComplete;
  return el;
}
async function sent(el: Element, click: () => void): Promise<OverviewWebviewToHost> {
  setTimeout(click);
  return ((await oneEvent(el, 'escurel-message')) as CustomEvent<OverviewWebviewToHost>).detail;
}

describe('<escurel-overview>', () => {
  it('shows the five tiles in the order a day starts, each with its answer in words', async () => {
    const el = await render();
    expect(qa(el, 'section.tile h2').map(text)).to.deep.equal([
      'Decisions waiting',
      'Agent activity',
      'Needs attention',
      'Open items',
      'Recently finished',
    ]);
    expect(qa(el, '.headline').map(text)).to.deep.equal([
      '2 waiting for you',
      'Agents are running · last seen just now',
      '1 needs a look',
      '3 kinds of work',
      'Last 24 h · 8 runs',
    ]);
  });

  it('marks what needs a person with a shape and a word, not only a colour', async () => {
    const el = await render();
    const decisions = qa(el, 'section.tile')[0]!;
    expect(decisions.classList.contains('tone-attention')).to.equal(true);
    expect(decisions.querySelector('svg')).to.not.equal(null);
    expect(text(decisions.querySelector('.headline'))).to.contain('waiting for you');
  });

  it('opens a line through its key, and names nothing else', async () => {
    const el = await render();
    const first = qa(el, '[data-key="decisions:0"]')[0] as HTMLButtonElement;
    const msg = await sent(el, () => first.click());
    expect(msg).to.deep.equal({ type: 'open', key: 'decisions:0' });
  });

  it('opens the whole view from a tile title', async () => {
    const el = await render();
    const title = qa(el, 'section.tile h2 button')[1] as HTMLButtonElement;
    const msg = await sent(el, () => title.click());
    expect(msg).to.deep.equal({ type: 'open-tile', tile: 'agents' });
  });

  it('says what is empty, and how many more there are than it shows', async () => {
    const quietEl = await render(quiet);
    expect(qa(quietEl, '.empty').map(text)).to.have.length(5);
    expect(text(qa(quietEl, '.empty')[0])).to.equal('Nothing needs your decision right now.');
    const el = await render();
    expect(text(qa(el, 'section.tile')[3]!.querySelector('.more'))).to.equal('+1 more');
  });

  it('offers the way out of the focus view, or the way in', async () => {
    const on = await render(morning);
    const out = q(on, 'button.focus-toggle') as HTMLButtonElement;
    expect(text(out)).to.equal('Leave focus view');
    expect(await sent(on, () => out.click())).to.deep.equal({ type: 'toggle-focus' });
    const off = await render(quiet);
    expect(text(q(off, 'button.focus-toggle'))).to.equal('Switch to focus view');
  });

  it('refreshes on request and says when it last read', async () => {
    const el = await render();
    expect(text(q(el, '.updated'))).to.match(/^Updated \d{2}:\d{2}$/);
    const msg = await sent(el, () => (q(el, 'button.refresh') as HTMLButtonElement).click());
    expect(msg).to.deep.equal({ type: 'refresh' });
  });

  it('shows an error with a way to try again, and a quiet loading state', async () => {
    const el = await fixture<EscurelOverview>(
      html`<escurel-overview error="The gateway could not be reached."></escurel-overview>`,
    );
    expect(text(q(el, '.error'))).to.contain('The gateway could not be reached.');
    const msg = await sent(el, () => (q(el, '.error button') as HTMLButtonElement).click());
    expect(msg).to.deep.equal({ type: 'refresh' });
    const loading = await fixture<EscurelOverview>(html`<escurel-overview></escurel-overview>`);
    expect(text(q(loading, '.loading'))).to.contain('Loading');
  });

  it('renders a gateway-written label as text, never as markup', async () => {
    const view = structuredClone(morning);
    view.tiles[0]!.items[0]!.label = '<img src=x onerror="window.__pwned=1">';
    const el = await render(view);
    expect(q(el, 'img')).to.equal(null);
    expect(text(qa(el, '[data-key="decisions:0"] .label')[0])).to.contain('<img');
  });
});
