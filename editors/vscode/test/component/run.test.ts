import { expect, fixture, html } from '@open-wc/testing';
import type { RunView, RunWebviewToHost } from '../../src/shared/protocol';
import '../../webview/run/main';
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
  it('renders recorded attempts, plan steps with status text, and tool calls', async () => {
    const el = await render();
    expect(text(q(el, '.attempt'))).to.contain('#1');
    expect(text(q(el, '.attempt'))).to.contain('ok');
    expect(text(q(el, '.attempt'))).to.contain('175 ms');
    expect(
      qa(el, '.plan-step').map((row) => [...row.querySelectorAll('span')].map(text).join(' ')),
    ).to.deep.equal([
      '✓ read the target page completed',
      '◐ draft the fold for review in progress',
    ]);
    const calls = qa(el, '.tool-call').map((row) =>
      [...row.querySelectorAll('span')].map(text).join(' '),
    );
    expect(calls.join(' ')).to.contain('1 · list_inbox · ok');
    expect(calls.join(' ')).to.contain('2 · expand · ok');
  });

  it('posts the recorded next_after and hides Load more on the last page', async () => {
    const el = await render();
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    (q(el, '.load-more') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'load-more-calls', after: 2 }]);
    el.view = recordedLastPage;
    await el.updateComplete;
    expect(q(el, '.load-more')).to.not.exist;
  });

  it('explains when a run reports tool calls but per-call rows are unavailable', async () => {
    // A verifier-less harness can report calls without run-bound rows; the recording has the count.
    const el = await render({ ...recordedRunView, calls: [], nextAfter: null });
    expect(text(q(el, '.calls-unavailable'))).to.equal(
      '4 tool calls reported; per-call detail is not available for this run',
    );
  });

  it('shows a reconnectable error and posts refresh', async () => {
    const el = await render();
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    el.error = { message: 'Connection lost', canReconnect: true };
    await el.updateComplete;
    expect(text(q(el, '.error'))).to.contain('Connection lost');
    (q(el, '.reconnect') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'refresh' }]);
  });

  it('posts the trace id for host clipboard handling', async () => {
    const el = await render();
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    (q(el, '.copy-trace') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([{ type: 'copy-trace-id', traceId: recordedRunView.traceId }]);
  });
});
