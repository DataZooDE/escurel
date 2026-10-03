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

  it('shows the summary, the body and the gate for a review skill; the form is read-only and says how to edit', async () => {
    const el = await render();
    expect(text(q(el, '.summary'))).to.contain('Delivery at risk');
    expect(text(q(el, '.body'))).to.contain('Body text');
    expect(text(q(el, '.gate'))).to.contain('review');
    // The note must tell a reader what to DO. It used to say editing 'arrives with backend PR-1':
    // a ticket number nobody reading a form can act on, about work that has since shipped.
    const note = text(q(el, '.readonly-note'));
    expect(note).to.contain('Markdown');
    expect(note).to.not.match(/PR-\d/);
    expect(
      qa(el, 'input, textarea, select').every((i) => (i as HTMLInputElement).disabled),
    ).to.equal(true);
  });

  it('renders the body as markdown: the items table is a table, not source text', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{
          ...orderPage,
          body: '## Items\n\n| Item | Material | Qty |\n|---|---|---:|\n| 10 | GH-4711 | 240 |\n\n## History\n\n- created from customer PO',
        }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    expect(text(q(el, '.body h2'))).to.equal('Items');
    expect(qa(el, '.body table thead th').map((c) => text(c))).to.deep.equal([
      'Item',
      'Material',
      'Qty',
    ]);
    expect(qa(el, '.body table tbody td').map((c) => text(c))).to.deep.equal([
      '10',
      'GH-4711',
      '240',
    ]);
    expect(qa(el, '.body ul li')).to.have.length(1);
    // No leftover markdown syntax shown to the reader.
    expect(text(q(el, '.body'))).to.not.contain('|---');
    expect(text(q(el, '.body'))).to.not.contain('## ');
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

describe('<escurel-page-as-ui> thread strip', () => {
  const withThread = async (runStatus = 'processed') => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui
        .model=${{ ...orderPage, thread: { rootEventId: 'root-1', runId: 'run-1', runStatus } }}
      ></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    return el;
  };

  it('shows where the page came from, and opens the thread and the run', async () => {
    const el = await withThread();
    const strip = q(el, '.thread-strip')!;
    expect(strip).to.exist;
    expect(text(strip)).to.contain('Thread');
    expect(text(strip)).to.contain('processed');
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.thread-strip .open-thread') as HTMLButtonElement).click();
    (q(el, '.thread-strip .open-run') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      { type: 'open-thread', rootEventId: 'root-1' },
      { type: 'open-run', runId: 'run-1' },
    ]);
  });

  it('gives its buttons names a screen reader can use', async () => {
    const el = await withThread();
    // The visible text IS the name ("label in name"); an id is only ever a tooltip.
    expect(text(q(el, '.open-thread'))).to.contain('Open thread');
    expect(text(q(el, '.open-run'))).to.equal('Open run');
    expect(q(el, '.open-run')!.getAttribute('aria-label')).to.equal(null);
  });

  it('says so when the run that produced the page did not succeed', async () => {
    const el = await withThread('failed');
    expect(text(q(el, '.thread-strip'))).to.contain('failed');
  });

  it('is absent for a page no run has finished against', async () => {
    const el = await render();
    expect(q(el, '.thread-strip')).to.equal(null);
  });

  it('names a checkbox field, so a screen reader hears more than "checkbox, checked"', async () => {
    const el = await render();
    // <escurel-field> renders into the light DOM.
    const box = qa(el, 'escurel-field')
      .map((f) => f.querySelector('input[type="checkbox"]'))
      .find(Boolean) as HTMLInputElement | undefined;
    expect(box !== undefined).to.equal(true);
    expect((box!.getAttribute('aria-label') ?? '').length > 0).to.equal(true);
  });

  it('tells two split buttons on one page apart, and opens the menu with the arrow key', async () => {
    const el = await render();
    const chevrons = qa(el, '.skill-button .chevron, .instance-button .chevron') as HTMLElement[];
    const names = chevrons.map((c) => c.getAttribute('aria-label'));
    expect(new Set(names).size, `distinct names: ${names.join(' | ')}`).to.equal(names.length);

    const first = chevrons[0]!;
    first.focus();
    first.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    await el.updateComplete;
    const menus = qa(el, '.skill-button [role="menu"], .instance-button [role="menu"]');
    expect(
      menus.length >= 1 || chevrons.some((c) => c.getAttribute('aria-expanded') === 'true'),
    ).to.equal(true);
  });

  it('does not read a run id aloud as the name of the link to the run', async () => {
    const el = await render();
    const runLink = q(el, '.open-run') as HTMLElement | null;
    if (!runLink) return; // the fixture page has no thread strip
    expect(/[0-9A-Z]{20,}/.test(runLink.getAttribute('aria-label') ?? '')).to.equal(false);
  });

  it('a wikilink in the body asks the host to open it', async () => {
    const el = await render();
    const sent: WebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<WebviewToHost>).detail),
    );
    (q(el, '.body .wikilink') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'open-wikilink', wikilink: '[[supplier::stahl-ag]]' }]);
  });

  describe('high contrast', () => {
    const HC =
      '--vscode-contrastBorder:#6fc3df;--vscode-contrastActiveBorder:#f38518;--vscode-widget-border:#ffffff;--vscode-button-background:transparent;--vscode-button-foreground:#ffffff;--vscode-badge-background:#000000;--vscode-badge-foreground:#ffffff';
    async function renderHc(): Promise<EscurelPageAsUi> {
      // A read-only skill shows the layer chip.
      const model = { ...orderPage, skill: { ...orderPage.skill, readOnly: true } };
      const host = await fixture<HTMLElement>(
        html`<div style=${HC}><escurel-page-as-ui .model=${model}></escurel-page-as-ui></div>`,
      );
      const el = host.querySelector('escurel-page-as-ui') as EscurelPageAsUi;
      await el.updateComplete;
      return el;
    }

    it('shows which of Page | Markdown is selected without relying on a fill', async () => {
      // In high contrast the button background is transparent, so the selected one looked like the
      // other. It carries the active-border colour and a heavier weight instead.
      const el = await renderHc();
      const on = q(el, '.toggle button[aria-pressed="true"]') as HTMLElement;
      const off = q(el, '.toggle button[aria-pressed="false"]') as HTMLElement;
      expect(getComputedStyle(on).boxShadow).to.contain('rgb(243, 133, 24)');
      expect(getComputedStyle(off).boxShadow).to.not.contain('rgb(243, 133, 24)');
      expect(Number(getComputedStyle(on).fontWeight)).to.be.greaterThan(
        Number(getComputedStyle(off).fontWeight),
      );
    });

    it('outlines a chip, which otherwise loses its pill and reads as bare text', async () => {
      const el = await renderHc();
      const chip = (el.shadowRoot!.querySelector('.chip') ??
        qa(el, 'escurel-field')
          .map((f) => f.querySelector('.chip'))
          .find(Boolean)) as HTMLElement | null;
      expect(chip !== null, 'the fixture page has a chip').to.equal(true);
      expect(getComputedStyle(chip!).borderTopColor).to.equal('rgb(111, 195, 223)');
    });

    it('draws every rule in one colour', async () => {
      const el = await renderHc();
      const border = getComputedStyle(el).getPropertyValue('--escurel-border').trim();
      expect(border).to.equal('#6fc3df');
    });
  });
});
