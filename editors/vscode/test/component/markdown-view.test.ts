import { expect } from '@open-wc/testing';
import { render } from 'lit';
import { renderMarkdown, type ChartImages } from '../../webview/shared/markdown-view';

function mount(src: string, charts?: ChartImages): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  render(renderMarkdown(src, charts), host);
  return host;
}

describe('renderMarkdown', () => {
  it('renders the order items as a real table with aligned numbers', () => {
    const host = mount(
      [
        '| Item | Material | Qty | Net value |',
        '|---|---|---:|---:|',
        '| 10 | GH-4711 | 240 | 62,400.00 |',
      ].join('\n'),
    );
    expect(Array.from(host.querySelectorAll('thead th')).map((c) => c.textContent)).to.deep.equal([
      'Item',
      'Material',
      'Qty',
      'Net value',
    ]);
    const cells = Array.from(host.querySelectorAll('tbody td'));
    expect(cells.map((c) => c.textContent)).to.deep.equal(['10', 'GH-4711', '240', '62,400.00']);
    expect(cells[2]!.classList.contains('align-right')).to.equal(true);
    expect(cells[3]!.classList.contains('align-right')).to.equal(true);
    expect(cells[0]!.classList.contains('align-right')).to.equal(false);
  });

  it('renders headings, lists, emphasis and code as elements', () => {
    const host = mount('## History\n\n- one\n- **two**\n\nUse `sales_doc`.');
    expect(host.querySelector('h2')?.textContent).to.equal('History');
    expect(host.querySelectorAll('ul > li')).to.have.length(2);
    expect(host.querySelector('li strong')?.textContent).to.equal('two');
    expect(host.querySelector('p code')?.textContent).to.equal('sales_doc');
  });

  it('links http(s) targets safely and never links a javascript: one', () => {
    const host = mount('[ok](https://sap.example/x) and [bad](javascript:alert(1))');
    const links = Array.from(host.querySelectorAll('a'));
    expect(links).to.have.length(1);
    expect(links[0]!.getAttribute('href')).to.equal('https://sap.example/x');
    expect(links[0]!.getAttribute('rel')).to.contain('noopener');
    expect(host.textContent).to.contain('javascript:alert(1)'); // shown as text, inert
  });

  it('puts raw HTML on the page as TEXT, never as elements', () => {
    const host = mount(
      '<script>window.__pwned = 1</script> <img src=x onerror="window.__pwned = 2">',
    );
    expect(host.querySelector('script')).to.equal(null);
    expect(host.querySelector('img')).to.equal(null);
    expect(host.textContent).to.contain('<script>');
    expect((window as unknown as { __pwned?: number }).__pwned).to.equal(undefined);
  });

  it('shows a wikilink as a button that asks the host to open the page it names', () => {
    const host = mount('see [[supplier::meier-guss|the vendor]]');
    const link = host.querySelector('button.wikilink') as HTMLButtonElement | null;
    expect(link !== null, 'a focusable button, not an inert span').to.equal(true);
    expect(link!.textContent?.trim()).to.equal('the vendor');
    expect(link!.getAttribute('title')).to.equal('supplier::meier-guss');
    expect(host.querySelector('a')).to.equal(null);

    // Resolving needs the gateway, so the webview only SAYS which link was chosen.
    const heard: string[] = [];
    host.addEventListener('escurel-wikilink', (e) => heard.push((e as CustomEvent<string>).detail));
    link!.click();
    expect(heard).to.deep.equal(['[[supplier::meier-guss]]']);
  });

  it('says where a link REALLY goes when its label looks like a different address', () => {
    // A phishing shape: the words say one site, the target is another.
    const host = mount(
      '[https://sap.example/login](https://evil.example/steal) and [docs](https://sap.example/d)',
    );
    const [bad, ok] = Array.from(host.querySelectorAll('a'));
    expect(bad!.getAttribute('title')).to.contain('evil.example');
    expect(bad!.classList.contains('link-mismatch')).to.equal(true);
    expect(ok!.getAttribute('title')).to.contain('sap.example');
    expect(ok!.classList.contains('link-mismatch')).to.equal(false);
  });

  describe('charts', () => {
    const body = '![Rows changed: a 3](evolve-chart:cmp-1/rows_changed)';
    const resolved: ChartImages = new Map([
      [
        'cmp-1/rows_changed',
        { src: 'data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=', alt: 'Rows changed: a 3' },
      ],
    ]);

    it('draws a resolved chart as an image with its description', () => {
      const host = mount(body, resolved);
      const img = host.querySelector('figure.chart img') as HTMLImageElement;
      expect(img.getAttribute('src')).to.equal('data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=');
      expect(img.getAttribute('alt')).to.equal('Rows changed: a 3');
    });

    it('shows only the description when the host resolved nothing', () => {
      const host = mount(body);
      expect(host.querySelector('img')).to.equal(null);
      expect(host.textContent).to.contain('Rows changed: a 3');
    });

    it('does not draw a chart the host did not resolve, even if another one was', () => {
      const host = mount('![x](evolve-chart:cmp-2/rows_changed)', resolved);
      expect(host.querySelector('img')).to.equal(null);
    });

    it('never draws an ordinary image, whatever the resolved map holds', () => {
      const host = mount('![logo](https://tracker.example/p.gif)', resolved);
      expect(host.querySelector('img')).to.equal(null);
      expect(host.textContent).to.contain('logo');
    });
  });
});
