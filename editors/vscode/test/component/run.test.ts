import { pageSlug } from '../../src/shared/pageId';
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
    expect(text(q(el, '.tool-call'))).to.contain('Read the inbox');
    // The raw tool name is for the tooltip.
    expect(q(el, '.call-tool')?.getAttribute('title')).to.equal('list_inbox');
  });

  it('draws a time axis over the calls and places each bar on it', async () => {
    const at = (secs: number) => `2026-09-29T04:59:${String(secs).padStart(2, '0')}Z`;
    const calls: RunView['calls'] = [
      { ...recordedRunView.calls[0]!, seq: 1, at: at(10), durationMs: 500 },
      { ...recordedRunView.calls[0]!, seq: 2, at: at(12), durationMs: 2000 },
    ];
    const el = await render({ ...recordedRunView, startedAt: at(10), calls });
    const axis = q(el, '.trace-axis');
    expect(axis, 'axis').to.not.equal(null);
    expect(axis!.getAttribute('aria-hidden')).to.equal('true');
    const labels = qa(el, '.trace-axis .tick').map((t) => text(t));
    expect(labels).to.deep.equal(['0', '1 s', '2 s', '3 s']);
    expect(text(q(el, '.trace-axis .axis-end'))).to.equal('4 s');
    const bars = qa(el, '.call-bar > span') as HTMLElement[];
    // the second call starts half-way and takes half of the 4 s
    expect(parseFloat(bars[1]!.style.left)).to.be.closeTo(50, 0.1);
    expect(parseFloat(bars[1]!.style.width)).to.be.closeTo(50, 0.1);
    expect(parseFloat(bars[0]!.style.left)).to.be.closeTo(0, 0.1);
  });

  it('shows no axis when the run has no timed calls', async () => {
    const el = await render({
      ...recordedRunView,
      calls: [{ ...recordedRunView.calls[0]!, at: 'not a time' }],
    });
    expect(q(el, '.trace-axis')).to.equal(null);
    expect(q(el, '.call-bar > span')).to.not.equal(null);
  });

  it('uses tone for a humanised dead letter status', async () => {
    // This failure state is hand-written because the recorded run completed.
    const el = await render({ ...recordedRunView, status: 'dead_letter', tone: 'failed' });
    expect(q(el, '.status-chip')?.classList.contains('failed')).to.equal(true);
    expect(text(q(el, '.status-chip'))).to.equal('gave up');
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
    expect(text(q(el, '.link'))).to.equal(pageSlug(recordedRunView.targetPageId!));
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

  it('heads the page with the skill and the page it ran on; the id is secondary', async () => {
    const el = await render({
      ...recordedRunView,
      skill: 'supplier-risk',
      targetPageId: 'markdown/instances/customer-order__order-4500123.md',
    } as RunView);
    const h1 = q(el, 'h1')!;
    expect(text(h1)).to.contain('supplier-risk on order-4500123');
    // The id is not in the heading at all: it is the tooltip of Copy run id.
    expect(q(el, 'button.copy-run')!.getAttribute('title')).to.equal(recordedRunView.runId);
  });

  it('does not show a step as in progress under a finished run', async () => {
    const el = await render({
      ...recordedRunView,
      status: 'processed',
      plan: [
        { step: 'read the order', status: 'completed' },
        { step: 'draft the fold', status: 'in_progress' },
      ],
    });
    const steps = qa(el, '.plan-step').map(text);
    expect(steps.some((t) => t.includes('in progress'))).to.equal(false);
    expect(steps.some((t) => t.includes('draft the fold') && t.includes('not finished'))).to.equal(
      true,
    );
  });

  it('keeps a live run’s step in progress', async () => {
    const el = await render({
      ...recordedRunView,
      status: 'running',
      plan: [{ step: 'draft the fold', status: 'in_progress' }],
    });
    expect(qa(el, '.plan-step').map(text).join(' ')).to.contain('in progress');
  });

  describe('wording and legibility', () => {
    it('keeps the 26-character run id out of the heading: it sits behind a Copy run id button', async () => {
      const el = await render({ ...recordedRunView, skill: 'supplier-risk' } as RunView);
      expect(text(q(el, 'h1')).includes(recordedRunView.runId)).to.equal(false);
      const copy = q(el, 'button.copy-run') as HTMLButtonElement;
      expect(text(copy)).to.equal('Copy run id');
      const sent: RunWebviewToHost[] = [];
      el.addEventListener('escurel-message', (e) =>
        sent.push((e as CustomEvent<RunWebviewToHost>).detail),
      );
      copy.click();
      expect(sent).to.deep.equal([{ type: 'copy-run-id' }]);
    });

    it('describes who ran it as a sentence, not "Harness echo · Autonomy review · Depth 0"', async () => {
      const el = await render({
        ...recordedRunView,
        harness: 'echo',
        autonomy: 'review',
        depth: 0,
      });
      const meta = text(q(el, '.meta'));
      expect(meta).to.contain('Run by the echo agent');
      expect(meta).to.not.contain('Harness');
      expect(meta).to.not.contain('Depth 0');
    });

    it('a run that has just begun says it is starting, not that nothing was reported', async () => {
      const el = await render({ ...recordedRunView, status: 'running', attempts: [], plan: [] });
      const muted = qa(el, '.muted').map(text).join(' | ');
      expect(muted).to.contain('Starting…');
      expect(muted).to.contain('has not reported a plan yet');
      expect(muted).to.not.contain('No attempts reported');
    });

    it('every status chip carries an icon of its own shape', async () => {
      for (const status of ['processed', 'running', 'failed', 'dead_letter']) {
        const el = await render({ ...recordedRunView, status });
        expect(q(el, '.status-chip svg') !== null, status).to.equal(true);
      }
    });

    // The pill was grey with green text (about 2.5:1 in light themes, 3:1 on dark teal).
    const themes: Record<string, string> = {
      light:
        '--vscode-editor-background:#ffffff;--vscode-foreground:#3b3b3b;--vscode-charts-green:#388a34;--vscode-errorForeground:#a1260d;--vscode-badge-background:#c4c4c4;--vscode-badge-foreground:#333',
      dark: '--vscode-editor-background:#1e1e1e;--vscode-foreground:#cccccc;--vscode-charts-green:#89d185;--vscode-errorForeground:#f48771;--vscode-badge-background:#4d4d4d;--vscode-badge-foreground:#fff',
    };
    for (const [name, tokens] of Object.entries(themes)) {
      it(`keeps the status text above 4.5:1 in ${name} (processed and failed)`, async () => {
        for (const status of ['processed', 'dead_letter']) {
          const host = await fixture<HTMLElement>(
            html`<div style=${tokens + ';background:var(--vscode-editor-background)'}>
              <escurel-run-detail
                .view=${{ ...recordedRunView, status, tone: status === 'processed' ? 'ok' : 'failed' }}
              ></escurel-run-detail>
            </div>`,
          );
          const el = host.querySelector('escurel-run-detail') as EscurelRunDetail;
          await el.updateComplete;
          const chip = el.shadowRoot!.querySelector('.status-chip') as HTMLElement;
          const cs = getComputedStyle(chip);
          const bg =
            cs.backgroundColor === 'rgba(0, 0, 0, 0)'
              ? getComputedStyle(host).backgroundColor
              : cs.backgroundColor;
          const ratio = contrast(cs.color, bg);
          expect(
            ratio >= 4.5,
            `${name} ${status}: ${cs.color} on ${bg} = ${ratio.toFixed(2)}`,
          ).to.equal(true);
        }
      });
    }
  });
});

function channel(v: number): number {
  const c = v / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}
function luminance(rgb: string): number {
  // color-mix() computes to `color(srgb 0.64 0.81 0.63)` (0..1), plain colours to `rgb(r, g, b)` (0..255).
  const scale = rgb.startsWith('color(') ? 255 : 1;
  const [r, g, b] = (rgb.match(/[\d.]+/g) ?? []).slice(0, 3).map((n) => Number(n) * scale);
  return 0.2126 * channel(r!) + 0.7152 * channel(g!) + 0.0722 * channel(b!);
}
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi! + 0.05) / (lo! + 0.05);
}

describe('<escurel-run-detail> navigation', () => {
  it('leads to the skill that ran and to the thread it belongs to', async () => {
    const el = await render({ ...recordedRunView, skill: 'supplier-risk' } as RunView);
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (event) =>
      sent.push((event as CustomEvent<RunWebviewToHost>).detail),
    );
    const skill = q(el, '.meta .view-skill') as HTMLButtonElement;
    expect(text(skill)).to.equal('View skill: supplier-risk');
    skill.click();
    (q(el, '.meta .open-thread') as HTMLButtonElement).click();
    // The thread button names no id: the host knows which thread this run belongs to.
    expect(sent).to.deep.equal([
      { type: 'view-skill', skill: 'supplier-risk' },
      { type: 'open-thread', rootEventId: '' },
    ]);
  });

  it('offers no skill link when the run does not know its skill', async () => {
    const noSkill: RunView = { ...recordedRunView };
    delete noSkill.skill;
    const el = await render(noSkill);
    expect(q(el, '.meta .view-skill')).to.equal(null);
  });

  describe('the trace', () => {
    const view: RunView = {
      ...recordedRunView,
      startedAt: '2026-10-04T12:00:00.000Z',
      producedPageId: 'markdown/instances/order/o1.md',
      calls: [
        {
          seq: 1,
          tool: 'read_page',
          status: 'ok',
          durationMs: 12.3,
          bytes: { request: 100, response: 2048 },
          at: '2026-10-04T12:00:01.000Z',
        },
        {
          seq: 2,
          tool: 'capture_event',
          status: 'error',
          errorCode: 'PERMISSION_DENIED',
          durationMs: 1500,
          bytes: { request: 300, response: 40 },
          at: '2026-10-04T12:00:03.000Z',
        },
      ],
    };

    it('is a timeline: tool, outcome in words, offset, duration in human units', async () => {
      const el = await render(view);
      const rows = qa(el, '.tool-call');
      expect(rows).to.have.length(2);
      expect(text(rows[0]!)).to.contain('Read a page');
      expect(text(rows[0]!)).to.contain('ok');
      expect(text(rows[0]!)).to.contain('+1 s');
      expect(text(rows[0]!)).to.contain('12 ms');
      expect(text(rows[1]!)).to.contain('failed');
      expect(text(rows[1]!)).to.contain('PERMISSION_DENIED');
      expect(text(rows[1]!)).to.contain('1.5 s');
      expect(
        text(el.shadowRoot!.querySelector('section[aria-label="What the agent did"]')),
      ).not.to.contain('request bytes');
    });

    it('expands a call to its sizes, and the summary row is keyboard operable', async () => {
      const el = await render(view);
      const first = qa(el, '.tool-call')[0] as HTMLDetailsElement;
      expect(first.tagName).to.equal('DETAILS');
      expect(first.open).to.equal(false);
      expect(text(first.querySelector('summary'))).to.contain('Read a page');
      expect(text(first.querySelector('.call-sizes'))).to.equal('sent 100 B · received 2 KB');
    });

    it('links to the draft the run produced, by asking the host (no id on the wire)', async () => {
      const el = await render(view);
      const sent: RunWebviewToHost[] = [];
      el.addEventListener('escurel-message', (event) =>
        sent.push((event as CustomEvent<RunWebviewToHost>).detail),
      );
      const open = q(el, 'button.open-produced') as HTMLButtonElement;
      expect(text(open)).to.contain('Open what this run produced');
      open.click();
      expect(sent).to.deep.equal([{ type: 'open-produced' }]);
    });

    it('has no produced link when the run produced nothing', async () => {
      const el = await render({ ...view, producedPageId: undefined });
      expect(q(el, 'button.open-produced')).to.equal(null);
    });
  });
});

describe('a failed run', () => {
  it('says why at the top, in full, and not only in a tooltip', async () => {
    const el = await fixture<EscurelRunDetail>(
      html`<escurel-run-detail
        .view=${{
          ...recordedRunView,
          status: 'dead_letter',
          tone: 'failed',
          failure: 'permanent — harness "refusing" is not allowed',
        }}
      ></escurel-run-detail>`,
    );
    await el.updateComplete;
    const banner = el.shadowRoot!.querySelector('.failure-banner') as HTMLElement;
    expect(banner.getAttribute('role')).to.equal('alert');
    expect(banner.textContent).to.contain('Gave up:');
    expect(banner.textContent).to.contain('harness "refusing" is not allowed');
  });
  it('has no banner for a run that did not fail', async () => {
    const el = await fixture<EscurelRunDetail>(
      html`<escurel-run-detail .view=${recordedRunView}></escurel-run-detail>`,
    );
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.failure-banner')).to.equal(null);
  });
  it('is honest about what the trace records', async () => {
    const el = await fixture<EscurelRunDetail>(
      html`<escurel-run-detail .view=${recordedRunView}></escurel-run-detail>`,
    );
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.timeline-note')!.textContent).to.contain(
      'not its arguments or its result',
    );
  });
});

describe('cancelling a run asks first, inline', () => {
  const running = (): RunView => ({
    ...recordedRunView,
    status: 'running',
    controls: [{ action: 'cancel', label: 'Cancel run', enabled: true, hint: 'Stops the run.' }],
  });
  it('shows the question and the consequence, and sends nothing until it is confirmed', async () => {
    const el = await fixture<EscurelRunDetail>(
      html`<escurel-run-detail .view=${running()}></escurel-run-detail>`,
    );
    await el.updateComplete;
    const sent: RunWebviewToHost[] = [];
    el.addEventListener('escurel-message', (e) =>
      sent.push((e as CustomEvent<RunWebviewToHost>).detail),
    );
    (el.shadowRoot!.querySelector('.run-control') as HTMLButtonElement).click();
    await el.updateComplete;
    const confirm = el.shadowRoot!.querySelector('.confirm')!;
    expect(confirm.textContent).to.contain('Cancel this run?');
    expect(confirm.textContent).to.contain('Work already done is kept');
    expect(sent).to.deep.equal([]);
    (el.shadowRoot!.querySelector('.confirm-no') as HTMLButtonElement).click();
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.confirm')).to.equal(null);
    expect(sent).to.deep.equal([]);
    (el.shadowRoot!.querySelector('.run-control') as HTMLButtonElement).click();
    await el.updateComplete;
    (el.shadowRoot!.querySelector('.confirm-yes') as HTMLButtonElement).click();
    expect(sent).to.deep.equal([
      { type: 'run-control', action: 'cancel', runId: recordedRunView.runId },
    ]);
  });
  it('does not ask before a retry, and the retry says what it does', async () => {
    const el = await fixture<EscurelRunDetail>(
      html`<escurel-run-detail
        .view=${{
          ...recordedRunView,
          status: 'failed',
          controls: [
            {
              action: 'retry',
              label: 'Retry',
              enabled: true,
              hint: 'Starts a new run. This attempt stays in history.',
            },
          ],
        }}
      ></escurel-run-detail>`,
    );
    await el.updateComplete;
    const btn = el.shadowRoot!.querySelector('.run-control') as HTMLButtonElement;
    expect(btn.getAttribute('title')).to.contain('stays in history');
    btn.click();
    await el.updateComplete;
    expect(el.shadowRoot!.querySelector('.confirm')).to.equal(null);
  });
});
