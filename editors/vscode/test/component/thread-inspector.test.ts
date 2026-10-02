import { expect, fixture, html, oneEvent } from '@open-wc/testing';
import type { ListLineageResponse } from '../../src/client/types';
import type { InspectorView, ThreadWebviewToHost } from '../../src/shared/protocol';
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

async function render(detail?: InspectorView, nodeId?: string): Promise<EscurelThreadInspector> {
  const el = await fixture<EscurelThreadInspector>(
    html`<escurel-thread-inspector .detail=${detail} .nodeId=${nodeId}></escurel-thread-inspector>`,
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

  it('shows skill buttons for an instance and posts start-skill and view-skill', async () => {
    const instanceDetail: InspectorView = {
      title: 'Order 4500123',
      rows: [],
      sideTitle: '',
      side: [],
      actions: {
        skills: {
          pageId: 'markdown/instances/order__4500123.md',
          actions: [
            { skill: 'reassess-risk', label: 'Reassess risk for Order 4500123 with an agent' },
          ],
        },
      },
    };

    const el = await render(instanceDetail, 'inst-1');
    const splitButton = el.shadowRoot!.querySelector('escurel-split-button');
    expect(splitButton).to.exist;

    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    // Primary click sends start-skill with background mode
    const primary = el.shadowRoot!.querySelector(
      '.skills .skill-button .primary',
    ) as HTMLButtonElement;
    expect(primary).to.exist;
    primary.click();

    expect(sent).to.deep.equal([
      {
        type: 'start-skill',
        skill: 'reassess-risk',
        pageId: 'markdown/instances/order__4500123.md',
        mode: 'background',
      },
    ]);
  });

  it('the split-button menu keeps its roles/keyboard behaviour', async () => {
    const instanceDetail: InspectorView = {
      title: 'Order 4500123',
      rows: [],
      sideTitle: '',
      side: [],
      actions: {
        skills: {
          pageId: 'markdown/instances/order__4500123.md',
          actions: [
            { skill: 'reassess-risk', label: 'Reassess risk for Order 4500123 with an agent' },
          ],
        },
      },
    };

    const el = await render(instanceDetail, 'inst-1');
    const chevron = el.shadowRoot!.querySelector(
      '.skills .skill-button .chevron',
    ) as HTMLButtonElement;
    expect(chevron.getAttribute('aria-haspopup')).to.equal('menu');
    chevron.click();
    await el.updateComplete;
    expect(chevron.getAttribute('aria-expanded')).to.equal('true');

    const menu = el.shadowRoot!.querySelector('.skills .skill-button [role="menu"]') as HTMLElement;
    expect(menu).to.exist;

    const closed = oneEvent(el, 'escurel-menu-closed');
    menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await closed;
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.skills .skill-button [role="menu"]')).to.not.exist;
  });

  it('shows a control bar for a run, disabled control is disabled with title, clicking posts message', async () => {
    const runDetail: InspectorView = {
      title: 'Echo run',
      rows: [],
      sideTitle: '',
      side: [],
      actions: {
        controls: [
          { action: 'retry', label: 'Retry', enabled: true },
          {
            action: 'requeue',
            label: 'Requeue',
            enabled: false,
            disabledReason: 'Only an admin can requeue a dead letter.',
          },
        ],
        skill: 'supplier-risk',
      },
    };

    const el = await render(runDetail, 'run-123');
    const controls = el.shadowRoot!.querySelectorAll('.control-button');
    expect(controls.length).to.equal(2);

    const retryBtn = controls[0] as HTMLButtonElement;
    expect(retryBtn.textContent?.trim()).to.equal('Retry');
    expect(retryBtn.disabled).to.be.false;

    const requeueBtn = controls[1] as HTMLButtonElement;
    expect(requeueBtn.textContent?.trim()).to.equal('Requeue');
    expect(requeueBtn.disabled).to.be.true;
    expect(requeueBtn.getAttribute('title')).to.equal('Only an admin can requeue a dead letter.');

    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    retryBtn.click();
    expect(sent).to.deep.equal([
      {
        type: 'run-control',
        action: 'retry',
        runId: 'run-123',
      },
    ]);
  });

  it('says WHY a control is deactivated in text, and ties the button to it', async () => {
    const el = await render(
      {
        title: 'Echo run',
        rows: [],
        sideTitle: '',
        side: [],
        actions: {
          controls: [
            { action: 'retry', label: 'Retry', enabled: true },
            {
              action: 'requeue',
              label: 'Requeue',
              enabled: false,
              disabledReason: 'Only an admin can requeue a dead letter.',
            },
          ],
          skill: 'supplier-risk',
        },
      },
      'run-123',
    );
    const hint = el.shadowRoot!.querySelector('.control-hint') as HTMLElement;
    expect(hint.textContent).to.contain('Only an admin can requeue a dead letter.');
    const requeue = el.shadowRoot!.querySelectorAll('.control-button')[1] as HTMLButtonElement;
    expect(requeue.getAttribute('aria-describedby')).to.equal(hint.id);
  });

  it('styles every control as secondary except Approve plan, as run detail does', async () => {
    const el = await render(
      {
        title: 'Echo run',
        rows: [],
        sideTitle: '',
        side: [],
        actions: {
          controls: [
            { action: 'approve', label: 'Approve plan', enabled: true },
            { action: 'retry', label: 'Retry', enabled: true },
          ],
          skill: 'supplier-risk',
        },
      },
      'run-123',
    );
    const [approve, retry] = Array.from(
      el.shadowRoot!.querySelectorAll('.control-button'),
    ) as HTMLButtonElement[];
    expect(approve!.classList.contains('primary')).to.equal(true);
    expect(retry!.classList.contains('primary')).to.equal(false);
  });
});
