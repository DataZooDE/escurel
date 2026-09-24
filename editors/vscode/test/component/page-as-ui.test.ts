import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/page-as-ui/main';
import type { EscurelPageAsUi } from '../../webview/page-as-ui/page-as-ui';
import type { WebviewToHost } from '../../src/shared/protocol';
import { orderPage } from './fixtures';

async function render(): Promise<EscurelPageAsUi> {
  const el = await fixture<EscurelPageAsUi>(
    html`<escurel-page-as-ui .model=${orderPage}></escurel-page-as-ui>`,
  );
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => Array.from(el.shadowRoot!.querySelectorAll(sel));
const text = (n: Element | null) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-page-as-ui>', () => {
  it('renders the header, the skill row and the Page | Markdown toggle', async () => {
    const el = await render();
    expect(text(q(el, 'h1'))).to.equal(orderPage.title);
    expect(text(q(el, '.skill-row'))).to.contain('customer-order');
    expect(qa(el, '.toggle button').map(text)).to.deep.equal(['Page', 'Markdown']);
  });

  it('renders every field by kind: badge, instance split button, money, date, bool, markdown', async () => {
    const el = await render();
    const rows = qa(el, '.field');
    expect(rows.map((r) => r.getAttribute('data-name'))).to.deep.equal([
      'status',
      'customer',
      'value_eur',
      'eta',
      'urgent',
      'notes',
    ]);
    expect(q(el, '.field[data-name="status"] .badge')).to.exist;
    expect(text(q(el, '.field[data-name="customer"] .instance-button .primary'))).to.equal(
      'hoffmann',
    );
    expect(
      q(el, '.field[data-name="customer"] .instance-button .primary')!.getAttribute('title'),
    ).to.equal('Open instance');
    expect(text(q(el, '.field[data-name="value_eur"] .value'))).to.equal('184,200.00');
    expect(q(el, '.field[data-name="urgent"] input[type="checkbox"]')).to.have.property(
      'checked',
      true,
    );
    expect(q(el, '.field[data-name="urgent"] input[type="checkbox"]')).to.have.property(
      'disabled',
      true,
    );
    expect(q(el, '.field[data-name="notes"] .markdown')).to.exist;
  });

  it('shows the summary, the body and the gate for a review skill; the form is read-only until PR-1', async () => {
    const el = await render();
    expect(text(q(el, '.summary'))).to.contain('Delivery at risk');
    expect(text(q(el, '.body'))).to.contain('Body text');
    expect(text(q(el, '.gate'))).to.contain('review');
    expect(
      qa(el, 'input, textarea, select').every((i) => (i as HTMLInputElement).disabled),
    ).to.equal(true);
  });

  it('renders the actions as Skill split buttons with the four-item menu', async () => {
    const el = await render();
    const buttons = qa(el, '.actions .skill-button');
    expect(buttons.map((b) => text(b.querySelector('.primary')))).to.deep.equal(
      orderPage.actions.map((a) => a.label),
    );
    (buttons[0]!.querySelector('.chevron') as HTMLButtonElement).click();
    await el.updateComplete;
    expect(
      qa(el, '.actions .skill-button [role="menu"] [role="menuitem"]').map(text),
    ).to.deep.equal([
      'Start in background',
      'First make a plan',
      'Start in terminal',
      'View skill',
    ]);
  });

  it('posts typed messages to the host: open instance, view skill, show raw, start skill', async () => {
    const el = await render();
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.field[data-name="customer"] .instance-button .primary') as HTMLButtonElement).click();
    (q(el, '.skill-row .skill-link') as HTMLButtonElement).click();
    (qa(el, '.toggle button')[1] as HTMLButtonElement).click();
    (q(el, '.actions .skill-button .primary') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      { type: 'open-wikilink', wikilink: '[[customer::hoffmann]]' },
      { type: 'view-skill', skill: 'customer-order' },
      { type: 'show-raw' },
      { type: 'start-skill', skill: 'supplier-risk', mode: 'background' },
    ]);
  });

  it('the split-button menu is keyboard-accessible and closes on Escape', async () => {
    const el = await render();
    const chevron = q(el, '.actions .skill-button .chevron') as HTMLButtonElement;
    expect(chevron.getAttribute('aria-haspopup')).to.equal('menu');
    chevron.click();
    await el.updateComplete;
    expect(chevron.getAttribute('aria-expanded')).to.equal('true');
    const menu = q(el, '.actions .skill-button [role="menu"]') as HTMLElement;
    const closed = oneEvent(el, 'escurel-menu-closed');
    menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await closed;
    await el.updateComplete;
    expect(q(el, '.actions .skill-button [role="menu"]')).to.not.exist;
  });
});
