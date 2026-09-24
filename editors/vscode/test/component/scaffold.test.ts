import { expect, fixture, html } from '@open-wc/testing';
import '../../webview/page-as-ui/main';

describe('scaffold', () => {
  it('renders the page-as-ui placeholder', async () => {
    const el = await fixture(html`<escurel-page-as-ui></escurel-page-as-ui>`);
    expect(el.shadowRoot?.textContent).to.contain('page-as-UI');
  });
});
