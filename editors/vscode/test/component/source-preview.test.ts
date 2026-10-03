import { expect, fixture, html } from '@open-wc/testing';
import type { PreviewModel } from '../../src/shared/preview';
import '../../webview/page-as-ui/source-preview';
import type { EscurelSourcePreview } from '../../webview/page-as-ui/source-preview';

async function render(preview: PreviewModel, resource?: string): Promise<EscurelSourcePreview> {
  const el = await fixture<EscurelSourcePreview>(
    html`<escurel-source-preview
      .preview=${preview}
      .resource=${resource}
    ></escurel-source-preview>`,
  );
  await el.updateComplete;
  return el;
}
const q = (el: Element, sel: string) => el.shadowRoot!.querySelector(sel);
const qa = (el: Element, sel: string) => [...el.shadowRoot!.querySelectorAll(sel)];
const text = (n: Element | null | undefined) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-source-preview>', () => {
  it('shows rows as a real table under a read-only (source) badge, with the source named', async () => {
    const el = await render({
      kind: 'rows',
      readOnly: true,
      source: 'vw_order_lines_all',
      columns: ['vbeln', 'netwr'],
      rows: [
        ['4500123', '62400'],
        ['4500131', '66200'],
      ],
      truncated: true,
    });
    expect(text(q(el, '.badge'))).to.contain('read-only (source)');
    expect(text(q(el, '.source'))).to.contain('vw_order_lines_all');
    expect(qa(el, 'thead th').map(text)).to.deep.equal(['vbeln', 'netwr']);
    expect(qa(el, 'tbody tr')).to.have.length(2);
    expect(text(qa(el, 'tbody tr')[1]!)).to.contain('66200');
    // Numbers line up; a truncated projection says so.
    expect(text(q(el, '.note'))).to.contain('more rows');
    expect(q(el, 'table')!.getAttribute('aria-label')).to.contain('vw_order_lines_all');
  });

  it('shows an empty source as a statement, not an empty table', async () => {
    const el = await render({
      kind: 'rows',
      readOnly: true,
      source: 'v',
      columns: [],
      rows: [],
      truncated: false,
    });
    expect(q(el, 'table')).to.equal(null);
    expect(text(q(el, '.empty'))).to.contain('no rows');
  });

  it('shows remote fields as name and value', async () => {
    const el = await render({
      kind: 'fields',
      readOnly: true,
      source: 'sap-api',
      fields: [{ name: 'status', value: 'open' }],
    });
    expect(text(q(el, 'dl'))).to.contain('status');
    expect(text(q(el, 'dl'))).to.contain('open');
  });

  it('shows document chunks, how many there are, and offers the original', async () => {
    const el = await render({
      kind: 'document',
      readOnly: true,
      chunks: [{ anchor: 'c1', text: 'First chunk.' }],
      total: 40,
      truncated: true,
    });
    expect(qa(el, '.chunk')).to.have.length(1);
    expect(text(q(el, '.note'))).to.contain('1 of 40');
    let asked = 0;
    el.addEventListener('open-original', () => (asked += 1));
    (q(el, 'button.open-original') as HTMLButtonElement).click();
    expect(asked).to.equal(1);
  });

  it('shows an issue as an alert with its code', async () => {
    const el = await render({
      kind: 'issue',
      readOnly: true,
      source: 'v',
      code: 'binding_degraded',
      message: 'source schema drifted',
    });
    const alert = q(el, '[role="alert"]');
    expect(text(alert)).to.contain('source schema drifted');
    expect(text(alert)).to.contain('binding_degraded');
  });

  it('never renders a value as markup', async () => {
    const el = await render({
      kind: 'rows',
      readOnly: true,
      source: 'v',
      columns: ['a'],
      rows: [['<img src=x onerror=alert(1)>']],
      truncated: false,
    });
    expect(q(el, 'img')).to.equal(null);
    expect(text(q(el, 'tbody td'))).to.contain('<img');
  });

  it('links the skill resource only when it is an http(s) address', async () => {
    const el = await render(
      { kind: 'rows', readOnly: true, source: 'v', columns: [], rows: [], truncated: false },
      'https://sap.example/vbak',
    );
    expect(q(el, 'a.resource')!.getAttribute('href')).to.equal('https://sap.example/vbak');
    const bad = await render(
      { kind: 'rows', readOnly: true, source: 'v', columns: [], rows: [], truncated: false },
      'javascript:alert(1)',
    );
    expect(q(bad, 'a.resource')).to.equal(null);
    expect(text(q(bad, '.resource'))).to.contain('javascript:alert(1)');
  });
});
