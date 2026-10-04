import { expect, fixture, html } from '@open-wc/testing';
import type { ListLineageResponse } from '../../src/client/types';
import type {
  DetailsWebviewToHost,
  InspectorView,
  ShownDetails,
  ThreadWebviewToHost,
} from '../../src/shared/protocol';
import { buildInspectors } from '../../src/thread/inspector';
import { focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import type { EscurelDetails } from '../../webview/details/details';
import type { EscurelThreadCanvas } from '../../webview/thread/thread-canvas';
import type { EscurelThreadInspector } from '../../webview/thread/inspector';
import '../../webview/details/details';
import '../../webview/thread/inspector';
import '../../webview/thread/thread-canvas';
import lineage from '../unit/fixtures/lineage/lineage-cascade.json';

const folded = foldLineage([lineage as ListLineageResponse]);
const view = toThreadView(folded);
const layout = layoutThread(view, new Set());
const focus = focusGraph(view, layout);
const inspectors = buildInspectors(view, [...folded.nodes.values()]);
const node = (kind: string) => view.nodes.find((n) => n.kind === kind)!;
const text = (n: Element | null | undefined) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();

// "Where can I find the analysis in the thread? I don't see the skills used." Two cards of one type
// must say which skill they belong to, and a node must link to what a person asks for next.
describe('thread cards say which skill they belong to', () => {
  async function canvas(): Promise<EscurelThreadCanvas> {
    const el = await fixture<EscurelThreadCanvas>(html`
      <escurel-thread-canvas .view=${view} .layout=${layout} .focus=${focus} .details=${{}}>
      </escurel-thread-canvas>
    `);
    await el.updateComplete;
    return el;
  }

  it('a page card carries the skill of its record next to the word "page"', async () => {
    const el = await canvas();
    const card = el.shadowRoot!.querySelector(`.card[data-node-id="${node('draft').id}"]`)!;
    expect(text(card.querySelector('.type-qualifier'))).to.equal('order');
    expect(card.getAttribute('aria-label')).to.contain('(order)');
  });

  it('a run card says which page it worked on', async () => {
    const el = await canvas();
    const card = el.shadowRoot!.querySelector(`.card[data-node-id="${node('run').id}"]`)!;
    expect(text(card.querySelector('.type-qualifier'))).to.equal('on o1');
  });
});

describe('the inspector links to where a node leads', () => {
  async function inspector(detail: InspectorView, nodeId: string): Promise<EscurelThreadInspector> {
    const el = await fixture<EscurelThreadInspector>(
      html`<escurel-thread-inspector
        .detail=${detail}
        .nodeId=${nodeId}
      ></escurel-thread-inspector>`,
    );
    await el.updateComplete;
    return el;
  }

  it('lists the run’s links in words and sends only the node and the kind', async () => {
    const run = node('run');
    const el = await inspector(inspectors[run.id]!, run.id);
    const labels = Array.from(el.shadowRoot!.querySelectorAll('.links .link-button')).map(text);
    expect(labels).to.deep.equal([
      'View skill: signal',
      'Open page: o1',
      'Open run',
      'Open thread',
    ]);
    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );
    (el.shadowRoot!.querySelector('.link-button[data-link="skill"]') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'open-link', nodeId: run.id, link: 'skill' }]);
  });

  it('a node with nowhere to go shows no link row', async () => {
    const el = await inspector({ title: 'x', rows: [], sideTitle: '', side: [] }, 'n');
    expect(el.shadowRoot!.querySelector('.links')).to.equal(null);
  });
});

describe('<escurel-details> forwards a link with the thread it was shown for', () => {
  it('wraps open-link, and explains how things connect when nothing is selected', async () => {
    const el = await fixture<EscurelDetails>(html`<escurel-details></escurel-details>`);
    await el.updateComplete;
    expect(text(el.shadowRoot!.querySelector('.empty'))).to.contain('How things connect');

    const run = node('run');
    el.shown = {
      rootEventId: 'root-A',
      nodeId: run.id,
      detail: inspectors[run.id]!,
    } as ShownDetails;
    await el.updateComplete;
    const inner = el.shadowRoot!.querySelector('escurel-thread-inspector')!;
    await (inner as unknown as { updateComplete: Promise<boolean> }).updateComplete;
    const sent: DetailsWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<DetailsWebviewToHost>).detail),
    );
    (inner.shadowRoot!.querySelector('.link-button[data-link="run"]') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      {
        type: 'details-action',
        rootEventId: 'root-A',
        message: { type: 'open-link', nodeId: run.id, link: 'run' },
      },
    ]);
  });
});
