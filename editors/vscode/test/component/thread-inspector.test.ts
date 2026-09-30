import { expect, fixture, html } from '@open-wc/testing';
import type { ListLineageResponse } from '../../src/client/types';
import type { InspectorView } from '../../src/shared/protocol';
import { buildInspectors } from '../../src/thread/inspector';
import { focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import type { EscurelThreadInspector } from '../../webview/thread/inspector';
import '../../webview/thread/inspector';
import lineage from '../unit/fixtures/lineage/lineage-cascade.json';

const folded = foldLineage([lineage as ListLineageResponse]);
const view = toThreadView(folded);
focusGraph(view, layoutThread(view, new Set()));
const details = buildInspectors(view, [...folded.nodes.values()]);

async function render(detail?: InspectorView): Promise<EscurelThreadInspector> {
  const el = await fixture<EscurelThreadInspector>(
    html`<escurel-thread-inspector .detail=${detail}></escurel-thread-inspector>`,
  );
  await el.updateComplete;
  return el;
}

describe('<escurel-thread-inspector>', () => {
  it('renders recorded rows, summary and side table with semantic markup', async () => {
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const el = await render(details[run.id]);
    expect(el.shadowRoot!.querySelector('h2')?.textContent).to.equal(run.title);
    expect(el.shadowRoot!.querySelector('dl')?.textContent).to.contain('echo');
    expect(el.shadowRoot!.querySelector('.body')?.textContent).to.contain('awaiting a human');
    expect(el.shadowRoot!.querySelector('.side')?.textContent).to.contain('Timing');
  });

  it('keeps state text when a row has a tone', async () => {
    const changeset = view.nodes.find((node) => node.kind === 'changeset')!;
    const el = await render(details[changeset.id]);
    const toned = el.shadowRoot!.querySelector('.tone-ok');
    expect(toned?.textContent).to.equal('promoted');
  });

  it('renders nothing without detail', async () => {
    const el = await render();
    expect(el.shadowRoot!.textContent?.trim()).to.equal('');
  });

  it('shows a hand-written HTML-looking summary as plain text', async () => {
    // The recording has plain summary text, so markup-shaped text must be supplied here.
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const el = await render({ ...details[run.id]!, body: '<b>not markup</b>' });
    expect(el.shadowRoot!.querySelector('.body')?.textContent).to.equal('<b>not markup</b>');
    expect(el.shadowRoot!.querySelector('.body b')).to.not.exist;
  });
});
