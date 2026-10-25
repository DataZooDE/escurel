import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/shared/augment-view';
import type { EscurelAugmentView } from '../../webview/shared/augment-view';
import type { PageAugmentView } from '../../src/shared/augment';
import '../../webview/page-as-ui/main';
import type { EscurelPageAsUi } from '../../webview/page-as-ui/page-as-ui';
import { orderPage } from './fixtures';

const view: PageAugmentView = {
  sections: [
    {
      id: 'impact',
      title: 'Impact',
      blocks: [
        { type: 'text', text: '<img src=x onerror=alert(1)> plain' },
        { type: 'facts', rows: [{ label: 'Open', value: '3', tone: 'attention' }] },
        { type: 'table', columns: ['Name', 'Days'], rows: [['A', '2'], ['B', '5']] },
        { type: 'list', items: [{ label: 'Open it', key: 'l:1' }, { label: 'Just text' }] },
      ],
    },
    { id: 'none', title: 'Nothing', blocks: [], empty: 'No rows.' },
  ],
  actions: [{ id: 'a:1', label: 'Go on', hint: 'Does it' }],
  forms: [
    {
      id: '0:note',
      title: 'Add a note',
      submitLabel: 'Propose',
      fields: [
        { name: 'text', label: 'Note', kind: 'textarea', required: true, maxLength: 50 },
        { name: 'kind', label: 'Kind', kind: 'enum', required: false, values: ['a', 'b'], maxLength: 10 },
        { name: 'flag', label: 'Flag', kind: 'bool', required: false, maxLength: 1 },
      ],
    },
  ],
};

const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => Array.from(el.shadowRoot!.querySelectorAll(sel));
const text = (n: Element | null) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

async function render(v: PageAugmentView = view): Promise<EscurelAugmentView> {
  const el = await fixture<EscurelAugmentView>(html`<escurel-augment .view=${v}></escurel-augment>`);
  await el.updateComplete;
  return el;
}

describe('<escurel-augment>', () => {
  it('draws sections, blocks and the empty line', async () => {
    const el = await render();
    expect(qa(el, 'section.augment-section h2').map(text)).to.deep.equal(['Impact', 'Nothing']);
    expect(text(q(el, '.facts'))).to.contain('Open');
    expect(text(q(el, '.facts'))).to.contain('3');
    expect(qa(el, 'table th').map(text)).to.deep.equal(['Name', 'Days']);
    expect(qa(el, 'table tbody tr')).to.have.length(2);
    expect(text(q(el, '.augment-empty'))).to.equal('No rows.');
  });

  it('shows untrusted text as text, never as markup', async () => {
    const el = await render();
    expect(q(el, 'img')).to.equal(null);
    expect(text(q(el, '.augment-text'))).to.contain('<img src=x');
  });

  it('says a tone in words as well as colour', async () => {
    const el = await render();
    expect(text(q(el, '.facts .tone'))).to.contain('needs attention');
  });

  it('a list line with a key is a button that posts the key; a line without is plain text', async () => {
    const el = await render();
    const buttons = qa(el, '.list button');
    expect(buttons.map(text)).to.deep.equal(['Open it']);
    setTimeout(() => (buttons[0] as HTMLButtonElement).click());
    const e = await oneEvent(el, 'augment-action');
    expect(e.detail).to.deep.equal({ id: 'l:1' });
  });

  it('an action button posts its id', async () => {
    const el = await render();
    const b = q(el, '.augment-actions button') as HTMLButtonElement;
    expect(b.textContent!.trim()).to.equal('Go on');
    expect(b.getAttribute('title')).to.equal('Does it');
    setTimeout(() => b.click());
    expect((await oneEvent(el, 'augment-action')).detail).to.deep.equal({ id: 'a:1' });
  });

  it('a form posts its values and refuses an empty required field in the page', async () => {
    const el = await render();
    const ta = q(el, 'form textarea') as HTMLTextAreaElement;
    const submit = q(el, 'form button[type="submit"]') as HTMLButtonElement;
    let fired = 0;
    el.addEventListener('augment-form', () => (fired += 1));
    submit.click();
    await el.updateComplete;
    expect(fired).to.equal(0);
    expect(text(q(el, 'form .field-error'))).to.contain('Note');
    ta.value = 'hello';
    ta.dispatchEvent(new Event('input'));
    (q(el, 'form select') as HTMLSelectElement).value = 'b';
    (q(el, 'form select') as HTMLSelectElement).dispatchEvent(new Event('change'));
    const flag = q(el, 'form input[type="checkbox"]') as HTMLInputElement;
    flag.checked = true;
    flag.dispatchEvent(new Event('change'));
    setTimeout(() => submit.click());
    const e = await oneEvent(el, 'augment-form');
    expect(e.detail).to.deep.equal({ id: '0:note', values: { text: 'hello', kind: 'b', flag: true } });
  });

  it('shows the result of a submit next to its form', async () => {
    const el = await render();
    el.result = { id: '0:note', ok: false, message: 'Refused.' };
    await el.updateComplete;
    expect(text(q(el, 'form .form-result'))).to.contain('Refused.');
  });

  it('says that something could not be read', async () => {
    const el = await render({ sections: [], actions: [], forms: [], unavailable: true });
    expect(text(q(el, '.augment-unavailable'))).to.contain('could not be read');
  });

  it('draws nothing for an empty view', async () => {
    const el = await render({ sections: [], actions: [], forms: [] });
    expect(q(el, 'section')).to.equal(null);
  });
});

describe('<escurel-page-as-ui> with an augment', () => {
  it('draws the companion additions and forwards their messages as page messages', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui .model=${{ ...orderPage, augment: view }}></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    const aug = q(el, 'escurel-augment') as EscurelAugmentView;
    expect(aug).to.exist;
    await aug.updateComplete;
    setTimeout(() => (aug.shadowRoot!.querySelector('.augment-actions button') as HTMLButtonElement).click());
    const e = await oneEvent(el, 'escurel-message');
    expect(e.detail).to.deep.equal({ type: 'augment-action', id: 'a:1' });
  });

  it('draws no augment element when the model has none', async () => {
    const el = await fixture<EscurelPageAsUi>(
      html`<escurel-page-as-ui .model=${orderPage}></escurel-page-as-ui>`,
    );
    await el.updateComplete;
    expect(q(el, 'escurel-augment')).to.equal(null);
  });
});
