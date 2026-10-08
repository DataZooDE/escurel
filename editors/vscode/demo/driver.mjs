// Plays the story that leaves a demo window ready for a walkthrough, against a running demo
// gateway and runner, and prints what it left behind as one line of JSON.
//
//   1. A supplier-risk signal about vendor Meier-Guss (a purchase-order confirmation moved by 14
//      days) arrives for sales order 4500123. The runner proposes a
//      change; it is PROMOTED here, which cascades: a follow-on event lands under the same run.
//      That thread is complete, and the walkthrough opens on it.
//   2. A second signal (a partial confirmation, 120 of 200 PC) arrives for sales order 4500131. The runner proposes a change and it is LEFT
//      OPEN, so Awaiting you has something to review live: diff, comment, promote.
import { readFileSync } from 'node:fs';

const [gatewayFile, bearerFile] = process.argv.slice(2);
const gw = JSON.parse(readFileSync(gatewayFile, 'utf8').split('\n')[0]);
const bearer = JSON.parse(readFileSync(bearerFile, 'utf8')).bearer;

async function call(name, args) {
  const res = await fetch(`${gw.gateway_url}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${bearer}` },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name, arguments: args },
    }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
  return body.result.structuredContent;
}

// A supplier document the demo uploads as a FILE (the document backend: text extracted and chunked,
// the original kept). Markdown on purpose: the extension never hands it to an application, it only
// saves it as plain text and shows where, which is what the e2e checks.
const FRAME_AGREEMENT = `# Frame agreement Meier-Guss GmbH, 2026

Supplier: Meier-Guss GmbH, Pforzheim (vendor 100234). Buyer: the plant at DE01.

## Scope

Cast gearbox housings (material GH-4711) and tool holders (TH-0815), called off against purchase orders.
The supplier is the sole source for GH-4711.

## Delivery terms

A confirmed delivery date may move by at most 7 days without the buyer's written consent. A move of more
than 7 days is a supply risk and is reported to purchasing the same day.

## Quality

Each delivery carries a material certificate. Rejected parts are replaced within 10 working days.

## Term

The agreement runs until 31 December 2026 and renews for one year unless either side gives notice three
months before it ends.
`;

async function uploadDocument(title, text, eventId) {
  const res = await fetch(`${gw.gateway_url}/ingest/upload`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${bearer}` },
    body: JSON.stringify({
      bytes_b64: Buffer.from(text).toString('base64'),
      content_type: 'text/markdown',
      title,
      event_id: eventId,
    }),
  });
  const body = await res.json();
  if (!res.ok || body.status !== 'materialised')
    throw new Error(`ingest ${title}: ${res.status} ${JSON.stringify(body)}`);
  return body.page_id;
}

async function until(what, f, ms = Number(process.env.ESCUREL_DEMO_WAIT_MS ?? 120_000)) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 400));
  }
}

const page = (id) => `markdown/instances/customer-order/${id}.md`;
const openChangesetOn = async (pageId) =>
  (await call('list_changesets', {})).changesets.find(
    (c) => c.status === 'open' && c.target_page_ids.includes(pageId),
  );

// 1. the complete thread
const a = await call('capture_event', {
  label_skill: 'supplier-risk',
  instance_page_id: page('order-4500123'),
  title: 'Vendor 100234 Meier-Guss: PO 4500087412 confirmation moved +14 days',
  body:
    'Purchasing (ME23N): vendor 100234 Meier-Guss GmbH, PO 4500087412 item 10, material GH-4711 ' +
    '(gearbox housing), 240 PC. Confirmation date moved from 2026-10-12 to 2026-10-26 (+14 days); ' +
    'vendor rating downgraded A to B. Affects sales order 4500123 item 10 ' +
    '(Hoffmann Automotive GmbH, customer PO HA-2026-0917, requested delivery 2026-10-12).',
  mime: 'text/plain',
  source: 'demo',
});
const csA = await until('the first changeset', () => openChangesetOn(page('order-4500123')));
await call('promote_changeset', { changeset_id: csA.changeset_id });
await until('the cascade hop', async () => {
  const l = await call('list_lineage', { root_event_id: a.event_id });
  return l.nodes.some((n) => n.type === 'event' && n.id !== a.event_id);
});
// The follow-on is an event too, and the runner gives it a run of its own. The echo harness
// folds the OLDEST inbox event that has a target page, so a second signal captured while that run
// is still going would be swallowed by it, and its changeset would hang under the wrong thread.
// Wait until nothing in the first thread is still running.
await until('the first thread to settle', async () => {
  const l = await call('list_lineage', { root_event_id: a.event_id });
  const runs = l.nodes.filter((n) => n.type === 'run');
  return runs.length >= 2 && runs.every((n) => n.state !== 'running');
});

// 2. the change waiting for a human
const b = await call('capture_event', {
  label_skill: 'supplier-risk',
  instance_page_id: page('order-4500131'),
  title: 'Vendor 100234 Meier-Guss: PO 4500087433 confirmed 120 of 200 PC',
  body:
    'Purchasing (ME23N): vendor 100234 Meier-Guss GmbH, PO 4500087433 item 20, material GH-4711 ' +
    '(gearbox housing). Confirmed 120 of 200 PC for 2026-10-19; the remaining 80 PC have no ' +
    'confirmed date. Affects sales order 4500131 item 20 (Kessler Werkzeugbau GmbH, customer PO ' +
    'KW-26-0443, requested delivery 2026-10-19).',
  mime: 'text/plain',
  source: 'demo',
});
const csB = await until('the second changeset', () => openChangesetOn(page('order-4500131')));

// Last, on purpose: the runner treats the upload like any signal (it dispatches the document skill), so
// uploading it while the story's runs are in flight made their timing depend on it.
const documentPage = await uploadDocument(
  'Frame agreement Meier-Guss 2026',
  FRAME_AGREEMENT,
  'demo-frame-agreement',
);

console.log(
  JSON.stringify({
    rootA: a.event_id,
    rootB: b.event_id,
    promoted: csA.changeset_id,
    awaiting: csB.changeset_id,
    document: documentPage,
  }),
);
