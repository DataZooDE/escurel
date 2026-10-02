import { expect, fixture, html } from '@open-wc/testing';
import type { RunView, RunWebviewToHost } from '../../src/shared/protocol';
import { buildRunView, mergeToolCallPage } from '../../src/runs/runModel';
import type { Event, GetRunToolCallsResponse, LineageNode } from '../../src/client';
import lineage from '../unit/fixtures/lineage/lineage-cascade.json';
import runEvents from '../unit/fixtures/lineage/run-events.json';
import page1 from '../unit/fixtures/lineage/run-tool-calls-page1.json';
import page2 from '../unit/fixtures/lineage/run-tool-calls-page2.json';
import { connectRunWebview } from '../../webview/run/main';
import type { EscurelRunDetail } from '../../webview/run/run-detail';
import { recordedLastPage, recordedRunView } from './run-fixtures';

async function render(view: RunView = recordedRunView): Promise<EscurelRunDetail> {
  const el = await fixture<EscurelRunDetail>(
    html`<escurel-run-detail .view=${view}></escurel-run-detail>`,
  );
  await el.updateComplete;
  return el;
}
const q = (el: Element, selector: string) => el.shadowRoot!.querySelector(selector);
const qa = (el: Element, selector: string) => [...el.shadowRoot!.querySelectorAll(selector)];
const text = (element: Element | null) => (element?.textContent ?? '').replace(/\s+/g, ' ').trim();

describe('<escurel-run-detail>', () => {
  it('renders header controls, explains disabled Requeue and posts the exact action', async () => {
    const view: RunView = {
      ...recordedRunView,
      status: 'dead_letter',
      controls: [
        { action: 'retry', label: 'Retry', enabled: true },
        {
          action: 'requeue',
          label: 'Requeue',
          enabled: false,
          disabledReason: 'Only an admin can requeue a dead letter.',
        },
      ],
    };
    const el = await render(view);
    const buttons = qa(el, 'header .run-control') as HTMLButtonElement[];
    expect(buttons.map((button) => text(button))).to.deep.equal(['Retry', 'Requeue']);
    expect(buttons[1]!.getAttribute('aria-disabled')).to.equal('true');
    expect(buttons[1]!.title).to.equal('Only an admin can requeue a dead letter.');
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    buttons[0]!.click();
    expect(sent).to.deep.equal([{ type: 'run-control', action: 'retry', runId: view.runId }]);
  });

  it('says WHY a control is deactivated in text, not only in a tooltip', async () => {
    // A tooltip does not reach a keyboard, a touch screen or a screen reader. The reason is on the
    // page, and the disabled button points at it.
    const view: RunView = {
      ...recordedRunView,
      status: 'dead_letter',
      controls: [
        { action: 'retry', label: 'Retry', enabled: true },
        {
          action: 'requeue',
          label: 'Requeue',
          enabled: false,
          disabledReason: 'Only an admin can requeue a dead letter.',
        },
      ],
    };
    const el = await render(view);
    const hint = q(el, '.control-hint') as HTMLElement;
    expect(text(hint)).to.contain('Only an admin can requeue a dead letter.');
    const requeue = (qa(el, 'header .run-control') as HTMLButtonElement[])[1]!;
    expect(requeue.getAttribute('aria-describedby')).to.equal(hint.id);
  });

  it('shows no hint when every control is available', async () => {
    const el = await render({
      ...recordedRunView,
      status: 'running',
      controls: [{ action: 'cancel', label: 'Cancel run', enabled: true }],
    });
    expect(q(el, '.control-hint') === null).to.equal(true);
  });

  it('keeps a deactivated control FOCUSABLE and announced, and sends nothing when it is clicked', async () => {
    // A natively disabled button is skipped by Tab and ignored by screen readers, so the reason
    // beside it would never reach the people who need it. aria-disabled keeps it in the tab order.
    const view: RunView = {
      ...recordedRunView,
      status: 'dead_letter',
      controls: [
        {
          action: 'requeue',
          label: 'Requeue',
          enabled: false,
          disabledReason: 'Only an admin can requeue a dead letter.',
        },
      ],
    };
    const el = await render(view);
    const requeue = q(el, 'header .run-control') as HTMLButtonElement;
    expect(requeue.disabled).to.equal(false);
    expect(requeue.tabIndex).to.not.equal(-1);
    expect(requeue.getAttribute('aria-disabled')).to.equal('true');
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    requeue.click();
    expect(sent).to.deep.equal([]);
  });

  it('sends a control once, however many times it is clicked before the run updates', async () => {
    const view: RunView = {
      ...recordedRunView,
      status: 'planned',
      controls: [{ action: 'approve', label: 'Approve plan', enabled: true }],
    };
    const el = await render(view);
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    const approve = q(el, 'header .run-control') as HTMLButtonElement;
    approve.click();
    approve.click();
    approve.click();
    expect(sent).to.have.length(1);
    // And it is available again once the host sends the run's next state.
    el.view = { ...view, status: 'processed', controls: [] };
    await el.updateComplete;
    expect(qa(el, 'header .run-control')).to.have.length(0);
  });

  it('draws no control group when the run offers nothing to do', async () => {
    const el = await render({ ...recordedRunView, status: 'processed', controls: [] });
    // A boolean, not the element: chai diffing a live DOM node on failure hangs the runner.
    expect(q(el, '.controls') === null).to.equal(true);
  });

  it('makes Approve plan primary and posts its run id', async () => {
    const view: RunView = {
      ...recordedRunView,
      status: 'planned',
      controls: [{ action: 'approve', label: 'Approve plan', enabled: true }],
    };
    const el = await render(view);
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    const button = q(el, 'header .run-control') as HTMLButtonElement;
    expect(button.classList.contains('primary')).to.equal(true);
    button.click();
    expect(sent).to.deep.equal([{ type: 'run-control', action: 'approve', runId: view.runId }]);
  });

  it('posts the host-provided skill for Fix skill', async () => {
    const view: RunView = {
      ...recordedRunView,
      skill: 'signal',
      status: 'failed',
      controls: [{ action: 'fix-skill', label: 'Fix skill', enabled: true }],
    };
    const el = await render(view);
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    (q(el, 'header .run-control') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'view-skill', skill: 'signal' }]);
  });

  it('builds the component fixture with the recorded run model and call pages', () => {
    const node = (lineage.nodes as LineageNode[]).find((item) => item.type === 'run');
    const model = buildRunView(node, runEvents.events as Event[]);
    expect(recordedRunView).to.deep.equal(
      mergeToolCallPage(model, page1 as GetRunToolCallsResponse),
    );
    expect(recordedLastPage).to.deep.equal(
      mergeToolCallPage(recordedRunView, page2 as GetRunToolCallsResponse),
    );
  });

  it('renders recorded attempts with dates, plan steps with status text, and tool calls', async () => {
    const el = await render();
    expect(text(q(el, '.attempt'))).to.contain('#1');
    expect(text(q(el, '.attempt'))).to.contain('2026-09-29 02:59:08 UTC');
    expect(text(q(el, '.attempt'))).to.contain('175 ms');
    expect(qa(el, '.plan-step').map((row) => text(row))).to.satisfy((rows: string[]) =>
      rows.some((row) => row.includes('read the target page') && row.includes('completed')),
    );
    expect(text(q(el, '.tool-call'))).to.contain('list_inbox');
  });

  it('uses tone for a humanised dead letter status', async () => {
    // This failure state is hand-written because the recorded run completed.
    const el = await render({ ...recordedRunView, status: 'dead_letter', tone: 'failed' });
    expect(q(el, '.status-chip')?.classList.contains('failed')).to.equal(true);
    expect(text(q(el, '.status-chip'))).to.equal('dead letter');
  });

  it('shows an error code on a failed tool call', async () => {
    // The recording has only successful calls, so this error row is hand-written.
    const errorCall: RunView['calls'][number] = {
      ...recordedRunView.calls[0]!,
      status: 'error',
      errorCode: 'PERMISSION_DENIED',
    };
    const el = await render({ ...recordedRunView, calls: [errorCall] });
    expect(text(q(el, '.tool-call'))).to.contain('PERMISSION_DENIED');
  });

  it('renders Load more only for a numeric cursor and sends it once per view', async () => {
    const el = await render();
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    const button = q(el, '.load-more') as HTMLButtonElement;
    button.click();
    button.click();
    await el.updateComplete;
    expect(sent).to.deep.equal([{ type: 'load-more-calls', after: 2 }]);
    expect(button.disabled).to.equal(true);
    el.view = { ...recordedRunView };
    await el.updateComplete;
    expect((q(el, '.load-more') as HTMLButtonElement).disabled).to.equal(false);
    el.view = recordedLastPage;
    await el.updateComplete;
    expect(q(el, '.load-more')).to.not.exist;
    // An undefined cursor is hand-written to cover an incomplete host payload.
    el.view = { ...recordedRunView, nextAfter: undefined as unknown as number };
    await el.updateComplete;
    expect(q(el, '.load-more')).to.not.exist;
  });

  it('gives target and trace buttons specific names and opens the target page', async () => {
    const el = await render();
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    // The visible text is the accessible name; the id is a tooltip, never read aloud in place of it.
    expect(text(q(el, '.link'))).to.equal(recordedRunView.targetPageId);
    expect(q(el, '.link')?.getAttribute('aria-label')).to.equal(null);
    expect(text(q(el, '.copy-trace'))).to.equal('Copy trace id');
    expect(q(el, '.copy-trace')?.getAttribute('aria-label')).to.equal(null);
    (q(el, '.link') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'open-page', pageId: recordedRunView.targetPageId }]);
  });

  it('renders a blocked plan step with its status and distinct class', async () => {
    // The recording has no blocked step, so this step is hand-written.
    const el = await render({
      ...recordedRunView,
      plan: [{ step: 'wait for review', status: 'blocked' }],
    });
    expect(q(el, '.plan-step')?.classList.contains('blocked')).to.equal(true);
    expect(text(q(el, '.plan-step'))).to.contain('wait for review');
    expect(text(q(el, '.step-status'))).to.equal('blocked');
  });

  it('shows loading and tells the user how to recover from a non-reconnectable error', async () => {
    const el = await fixture<EscurelRunDetail>(html`<escurel-run-detail></escurel-run-detail>`);
    expect(text(q(el, '[role="status"]'))).to.equal('Loading run…');
    el.error = { message: 'Run unavailable', canReconnect: false };
    await el.updateComplete;
    expect(text(q(el, '[role="alert"]'))).to.contain('Close this panel and open the run again.');
  });

  it('routes ready, loading, run, and error messages while ignoring unknown types', async () => {
    const sent: RunWebviewToHost[] = [];
    const api = { postMessage: (message: RunWebviewToHost) => sent.push(message) };
    const el = await fixture<EscurelRunDetail>(html`<escurel-run-detail></escurel-run-detail>`);
    const disconnect = connectRunWebview(api, el);
    try {
      expect(sent).to.deep.equal([{ type: 'ready' }]);
      window.dispatchEvent(
        new MessageEvent('message', { data: { type: 'run', view: recordedRunView } }),
      );
      await el.updateComplete;
      expect(el.view).to.equal(recordedRunView);
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { type: 'run-error', message: 'Lost', canReconnect: true },
        }),
      );
      await el.updateComplete;
      expect(el.error?.message).to.equal('Lost');
      window.dispatchEvent(new MessageEvent('message', { data: { type: 'unknown' } }));
      expect(el.error?.message).to.equal('Lost');
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { type: 'run-loading', runId: recordedRunView.runId },
        }),
      );
      await el.updateComplete;
      expect(el.view).to.equal(undefined);
      expect(el.error).to.equal(undefined);
    } finally {
      disconnect();
    }
  });

  it('names the copy and open buttons by what they do, and keeps ids out of the accessible name', async () => {
    const el = await render();
    const copy = el.shadowRoot!.querySelector('.copy-trace') as HTMLElement | null;
    const open = el.shadowRoot!.querySelector('button.link') as HTMLElement | null;
    expect(copy !== null && open !== null, 'the recorded run has a trace and a target').to.equal(
      true,
    );
    for (const b of [copy, open]) {
      expect(/[0-9a-f]{16,}|[0-9A-Z]{20,}/.test(b!.getAttribute('aria-label') ?? '')).to.equal(
        false,
      );
    }
  });
});
