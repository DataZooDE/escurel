import '../../../webview/run/main';
import { runControls } from '../../../src/runs/controls';
import type { EscurelRunDetail } from '../../../webview/run/run-detail';
import { recordedRunView } from '../../component/run-fixtures';

const el = document.querySelector('escurel-run-detail') as EscurelRunDetail;

// `?state=` shows the run in a state that offers controls, as a NON-admin sees it: Requeue is
// there but deactivated, with its reason. Without it, the recorded run as it was.
// The recording's tool-call clock runs 2 h ahead of its run clock (see
// docs/notes/discovered/2026-10-04-recorded-tool-call-clock-skew.md). Start the run at its first call so offsets read sanely.
const recorded = {
  ...recordedRunView,
  startedAt: recordedRunView.calls[0]?.at ?? recordedRunView.startedAt,
};
const state = new URLSearchParams(location.search).get('state');
// `?state=trace` is a finished run with a failed call and a produced draft: the readable trace.
const traceView = {
  ...recorded,
  skill: 'supplier-risk',
  startedAt: '2026-10-04T12:00:00.000Z',
  producedPageId: 'markdown/instances/supplier-risk-analysis__order-4500123.md',
  calls: [
    {
      seq: 1,
      tool: 'read_page',
      status: 'ok',
      durationMs: 12.3,
      bytes: { request: 120, response: 4096 },
      at: '2026-10-04T12:00:01.000Z',
    },
    {
      seq: 2,
      tool: 'search',
      status: 'ok',
      durationMs: 380,
      bytes: { request: 90, response: 18200 },
      at: '2026-10-04T12:00:02.000Z',
    },
    {
      seq: 3,
      tool: 'capture_event',
      status: 'error',
      errorCode: 'PERMISSION_DENIED',
      durationMs: 1500,
      bytes: { request: 300, response: 40 },
      at: '2026-10-04T12:00:04.000Z',
    },
  ],
};
// `?state=trace-detail` is the same run where the gateway kept what each call asked and got back,
// with the first and the failed step opened.
const detailView = {
  ...traceView,
  calls: traceView.calls.map((c) => ({
    ...c,
    argsSummary:
      c.seq === 1
        ? '{"page_id":"markdown/instances/customer-order/order-4500123.md"}'
        : c.seq === 3
          ? '{"source":"agent","label_skill":"supplier-risk","instance_page_id":"markdown/instances/customer-order/order-4500123.md","title":"risk report","body":"[2210 bytes]","provenance":{"api_token":"[redacted]"}}'
          : '{"query":"supplier risk Hoffmann","granularity":"page","limit":5}',
    resultSummary:
      c.seq === 3
        ? 'forbidden: you may not write this page'
        : '{"page":{"skill":"customer-order","id":"order-4500123"},"frontmatter":{"status":"open","sold_to":"Hoffmann GmbH"}}',
  })),
};
el.view =
  state === 'trace-detail'
    ? detailView
    : state === 'trace'
      ? traceView
      : state
        ? {
            ...recorded,
            status: state,
            tone: state === 'dead_letter' || state === 'failed' ? 'failed' : 'run',
            skill: 'supplier-risk',
            controls: runControls(state, 'not-admin'),
            ...(state === 'dead_letter' || state === 'failed'
              ? { failure: 'permanent — harness "refusing" is not allowed for this skill' }
              : {}),
          }
        : recorded;
if (state === 'trace-detail') {
  void el.updateComplete.then(() => {
    const rows = el.shadowRoot!.querySelectorAll<HTMLDetailsElement>('.tool-call');
    rows[0]!.open = true;
    rows[2]!.open = true;
  });
}
