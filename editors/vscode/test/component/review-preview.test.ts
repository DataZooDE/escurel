import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import '../../webview/review-preview/main';
import type { EscurelReviewPreview } from '../../webview/review-preview/review-preview';
import type { PageAugmentView } from '../../src/shared/augment';

const view: PageAugmentView = {
  sections: [
    {
      id: 'impact',
      title: 'Impact of this change',
      blocks: [
        { type: 'facts', rows: [{ label: 'Pages affected', value: '4' }] },
        { type: 'list', items: [{ label: 'Open the first', key: 'p:1' }] },
      ],
    },
  ],
  actions: [],
  forms: [],
};

describe('<escurel-review-preview>', () => {
  it('draws the sections a companion hook added', async () => {
    const el = await fixture<EscurelReviewPreview>(
      html`<escurel-review-preview .view=${view}></escurel-review-preview>`,
    );
    await el.updateComplete;
    const aug = el.shadowRoot!.querySelector('escurel-augment') as HTMLElement & { updateComplete: Promise<unknown> };
    await aug.updateComplete;
    expect(aug.shadowRoot!.querySelector('h2')!.textContent).to.equal('Impact of this change');
  });

  it('forwards a list line as an augment-action message and nothing else', async () => {
    const el = await fixture<EscurelReviewPreview>(
      html`<escurel-review-preview .view=${view}></escurel-review-preview>`,
    );
    const aug = el.shadowRoot!.querySelector('escurel-augment') as HTMLElement & { updateComplete: Promise<unknown> };
    await aug.updateComplete;
    setTimeout(() => (aug.shadowRoot!.querySelector('.list button') as HTMLButtonElement).click());
    const e = await oneEvent(el, 'escurel-message');
    expect(e.detail).to.deep.equal({ type: 'augment-action', id: 'p:1' });
  });

  it('has no form: a preview is read-only', async () => {
    const el = await fixture<EscurelReviewPreview>(
      html`<escurel-review-preview
        .view=${{ ...view, forms: [{ id: '0:x', title: 'X', submitLabel: 'Go', fields: [] }] }}
      ></escurel-review-preview>`,
    );
    const aug = el.shadowRoot!.querySelector('escurel-augment') as HTMLElement & { updateComplete: Promise<unknown> };
    await aug.updateComplete;
    expect(aug.shadowRoot!.querySelector('form')).to.equal(null);
  });

  it('says there is nothing to show before the host answers', async () => {
    const el = await fixture<EscurelReviewPreview>(html`<escurel-review-preview></escurel-review-preview>`);
    expect(el.shadowRoot!.textContent).to.contain('Loading');
  });
});
