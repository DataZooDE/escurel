import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/skill-page/main';
import type { EscurelSkillPage } from '../../webview/skill-page/skill-page';
import type { SkillPageToHost } from '../../src/shared/skillPage';
import { orderSkillPage } from './skill-fixtures';

async function render(): Promise<EscurelSkillPage> {
  const el = await fixture<EscurelSkillPage>(
    html`<escurel-skill-page .model=${orderSkillPage}></escurel-skill-page>`,
  );
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => Array.from(el.shadowRoot!.querySelectorAll(sel));
const text = (n: Element | null) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-skill-page>', () => {
  it('reads as a page: title, what it is for, freshness in words', async () => {
    const el = await render();
    expect(text(q(el, 'h1'))).to.equal('Customer order');
    expect(text(q(el, '.lede'))).to.equal('One order per customer purchase.');
    expect(text(q(el, '.stale-badge'))).to.equal('Stale');
    expect(text(q(el, '.provenance'))).to.contain('verified 2026-01-10');
  });

  it('shows role, folder, tags, backend as a list of facts', async () => {
    const el = await render();
    const dt = qa(el, 'dl.facts dt').map(text);
    expect(dt).to.include.members(['Role', 'Folder', 'Tags', 'Data from']);
    expect(text(qa(el, 'dl.facts dd')[dt.indexOf('Folder')]!)).to.equal('sales/orders');
  });

  it('lists the fields with required/optional and what they hold', async () => {
    const el = await render();
    const rows = qa(el, '.fields tbody tr').map(text);
    expect(rows[0])
      .to.contain('Customer')
      .and.to.contain('required')
      .and.to.contain('link to customer');
    expect(rows[1]).to.contain('optional').and.to.contain('one of open, shipped, closed');
  });

  it('lists records and recent runs, each with a link that asks the host to open it', async () => {
    const el = await render();
    expect(qa(el, '.instances .link').map(text)).to.deep.equal(['Order 4500123', 'order-4500124']);
    expect(qa(el, '.runs .state').map(text)).to.deep.equal(['waiting', 'done']);

    setTimeout(() => (qa(el, '.instances .link')[0] as HTMLButtonElement).click());
    const open = (await oneEvent(el, 'escurel-message')) as CustomEvent<SkillPageToHost>;
    expect(open.detail).to.deep.equal({
      type: 'open-page',
      pageId: 'markdown/instances/customer-order__order-4500123.md',
    });

    setTimeout(() => (q(el, '.runs .open-run') as HTMLButtonElement).click());
    const run = (await oneEvent(el, 'escurel-message')) as CustomEvent<SkillPageToHost>;
    expect(run.detail).to.deep.equal({ type: 'open-run', runId: '01RUNDONE' });
  });

  it('offers Show Markdown and the skill’s follow-ups', async () => {
    const el = await render();
    setTimeout(() => (q(el, '.show-markdown') as HTMLButtonElement).click());
    const raw = (await oneEvent(el, 'escurel-message')) as CustomEvent<SkillPageToHost>;
    expect(raw.detail).to.deep.equal({ type: 'show-raw' });

    expect(text(q(el, '.follow-ups button'))).to.equal('Check credit');
    setTimeout(() => (q(el, '.follow-ups button') as HTMLButtonElement).click());
    const start = (await oneEvent(el, 'escurel-message')) as CustomEvent<SkillPageToHost>;
    expect(start.detail).to.deep.equal({ type: 'start-skill', skill: 'credit-check', mode: 'run' });
  });

  it('collapses what is empty into one line instead of three empty sections', async () => {
    const el = await fixture<EscurelSkillPage>(
      html`<escurel-skill-page
        .model=${{
          ...orderSkillPage,
          fields: [],
          instances: { items: [], more: false },
          runs: [],
          actions: [],
        }}
      ></escurel-skill-page>`,
    );
    await el.updateComplete;
    expect(q(el, '.fields') === null).to.equal(true);
    expect(q(el, '.instances') === null).to.equal(true);
    expect(q(el, '.runs') === null).to.equal(true);
    expect(q(el, '.follow-ups') === null).to.equal(true);
    expect(text(q(el, '.nothing-yet'))).to.equal(
      'No fields declared, no records yet and no runs yet.',
    );
  });

  it('puts Show Markdown beside the title, and gives the follow-up the primary look', async () => {
    const el = await render();
    expect(q(el, '.title-row .show-markdown') !== null).to.equal(true);
    expect(q(el, '.follow-ups button.primary') !== null).to.equal(true);
  });

  it('explains a fact in a tooltip', async () => {
    const el = await render();
    const dt = qa(el, 'dl.facts dt').find((n) => text(n) === 'Data from') as HTMLElement;
    expect(dt.getAttribute('title')).to.contain('stored or read from');
  });

  it('says on the page, not in a toast, that a report is never run', async () => {
    const el = await fixture<EscurelSkillPage>(
      html`<escurel-skill-page
        .model=${{
          ...orderSkillPage,
          facts: [{ label: 'Role', value: 'report' }],
        }}
      ></escurel-skill-page>`,
    );
    await el.updateComplete;
    expect(text(q(el, '.report-note'))).to.contain('never run');
    const plain = await render();
    expect(q(plain, '.report-note') === null).to.equal(true);
  });
});
