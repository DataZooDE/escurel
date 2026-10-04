import { expect } from '@open-wc/testing';
import { render } from 'lit';
import { renderMarkdown } from '../../webview/shared/markdown-view';

function mount(src: string): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  render(renderMarkdown(src), host);
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
});
