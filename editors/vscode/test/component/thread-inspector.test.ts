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
    expect(el.shadowRoot!.querySelector('details.tech')?.textContent).to.contain('echo');
    expect(el.shadowRoot!.querySelector('.body')?.textContent).to.contain('awaiting a human');
    expect(el.shadowRoot!.querySelector('.side')?.textContent).to.contain('Timing');
  });

  it('keeps state text when a row has a tone', async () => {
    const changeset = view.nodes.find((node) => node.kind === 'changeset')!;
    const el = await render(details[changeset.id]);
    const toned = el.shadowRoot!.querySelector('.tone-ok');
    expect(toned?.textContent).to.equal('Applied');
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
    expect(retryBtn.getAttribute('aria-disabled')).to.equal(null);

    const requeueBtn = controls[1] as HTMLButtonElement;
    expect(requeueBtn.textContent?.trim()).to.equal('Requeue');
    // aria-disabled, not `disabled`: the button stays in the tab order, so a keyboard or screen
    // reader user can reach it and hear WHY it is deactivated (the hint it points at).
    expect(requeueBtn.disabled).to.be.false;
    expect(requeueBtn.getAttribute('aria-disabled')).to.equal('true');
    expect(requeueBtn.getAttribute('title')).to.equal('Only an admin can requeue a dead letter.');

    const sent: ThreadWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<ThreadWebviewToHost>).detail),
    );

    requeueBtn.click();
    expect(sent, 'a deactivated control does nothing when clicked').to.deep.equal([]);

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

  it('groups its buttons instead of claiming a toolbar it has no arrow-key roving for', async () => {
    const el = await render(
      {
        title: 'Echo run',
        rows: [],
        sideTitle: '',
        side: [],
        actions: { controls: [{ action: 'retry', label: 'Retry', enabled: true }], skill: 's' },
      },
      'run-1',
    );
    const roles = Array.from(el.shadowRoot!.querySelectorAll('.actions')).map((n) =>
      n.getAttribute('role'),
    );
    expect(roles.every((r) => r === 'group')).to.equal(true);
  });

  describe('what a person reads first', () => {
    const detail: InspectorView = {
      title: 'supplier-risk',
      kindLabel: 'Agent run',
      summary: 'The agent finished in 6 s.',
      rows: [
        { k: 'state', v: 'processed' },
        {
          k: 'trace_id',
          v: '01a0eb194a26f1510f9b86c9f8a6cd96aabbccddeeff00112233445566778899',
          tech: true,
        },
        { k: 'harness', v: 'echo', tech: true },
      ],
      sideTitle: 'Timing',
      side: [{ k: 'duration', v: '6 s' }],
    };

    it('opens with the kind, the title and ONE sentence, before any field', async () => {
      const el = await render(detail, 'run-1');
      const root = el.shadowRoot!;
      expect(root.querySelector('.kind')?.textContent?.trim()).to.equal('Agent run');
      expect(root.querySelector('h2')?.textContent?.trim()).to.equal('supplier-risk');
      const summary = root.querySelector('.summary') as HTMLElement;
      expect(summary.textContent).to.contain('The agent finished in 6 s.');
      const order = (a: Element, b: Element) =>
        !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
      expect(order(summary, root.querySelector('dl')!)).to.equal(true);
    });

    it('a node that needs the person says so in words, not only in colour', async () => {
      const el = await render(
        { ...detail, needsYou: true, summary: '2 changes are waiting for your review.' },
        'cs',
      );
      const badge = el.shadowRoot!.querySelector('.summary .needs');
      expect(badge?.textContent?.trim()).to.equal('Needs you');
    });

    it('keeps engineer fields (trace id, harness) under a collapsed Technical details', async () => {
      const el = await render(detail, 'run-1');
      const tech = el.shadowRoot!.querySelector('details.tech') as HTMLDetailsElement;
      expect(tech.open).to.equal(false);
      expect(tech.querySelector('summary')?.textContent?.trim()).to.equal('Technical details');
      const keys = Array.from(tech.querySelectorAll('dt')).map((n) => n.textContent?.trim());
      expect(keys).to.deep.equal(['trace_id', 'harness']);
      // The plain fields stay out of it.
      const plain = Array.from(el.shadowRoot!.querySelectorAll('.cols dt')).map((n) =>
        n.textContent?.trim(),
      );
      expect(plain).to.include('state');
      expect(plain).to.not.include('trace_id');
    });

    it('cuts the middle of a long id, keeps the whole value in the tooltip, and offers Copy', async () => {
      const el = await render(detail, 'run-1');
      const dd = el.shadowRoot!.querySelector('details.tech dd') as HTMLElement;
      const text = dd.querySelector('.id')!.textContent!.trim();
      expect(text.length < 40).to.equal(true);
      expect(text).to.contain('…');
      expect(dd.querySelector('.id')!.getAttribute('title')).to.contain('01a0eb194a26f151');
      const copy = dd.querySelector('button.copy') as HTMLButtonElement;
      expect(copy.getAttribute('aria-label')).to.equal('Copy trace_id');
    });
  });
});

describe('cancelling from the Details panel asks first, inline', () => {
  it('shows the question and posts nothing until it is confirmed', async () => {
    const el = await fixture<EscurelThreadInspector>(
      html`<escurel-thread-inspector
        .detail=${
          {
            title: 'supplier-risk',
            rows: [],
            side: [],
            sideTitle: '',
            actions: {
              controls: [{ action: 'cancel', label: 'Cancel run', enabled: true }],
              skill: 's',
            },
          } as InspectorView
        }
        .nodeId=${'run-1'}
      ></escurel-thread-inspector>`,
    );
    await el.updateComplete;
    const sent: unknown[] = [];
    el.addEventListener('escurel-message', (e) => sent.push((e as CustomEvent).detail));
    (el.shadowRoot!.querySelector('.control-button') as HTMLButtonElement).click();
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.confirm')!.textContent).to.contain('Cancel this run?');
    expect(sent).to.deep.equal([]);
    (el.shadowRoot!.querySelector('.confirm-yes') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'run-control', action: 'cancel', runId: 'run-1' }]);
  });
});
