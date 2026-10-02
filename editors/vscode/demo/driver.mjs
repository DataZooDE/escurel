// Plays the story that leaves a demo window ready for a walkthrough, against a running demo
// gateway and runner, and prints what it left behind as one line of JSON.
//
//   1. A supplier-risk signal about Meier-Guss arrives for order 4500123. The runner proposes a
//      change; it is PROMOTED here, which cascades: a follow-on event lands under the same run.
//      That thread is complete, and the walkthrough opens on it.
//   2. A second signal arrives for order 4500131. The runner proposes a change and it is LEFT
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

async function until(what, f, ms = 120_000) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 400));
  }
}

const page = (id) => `markdown/instances/customer-order__${id}.md`;
const openChangesetOn = async (pageId) =>
  (await call('list_changesets', {})).changesets.find(
    (c) => c.status === 'open' && c.target_page_ids.includes(pageId),
  );

// 1. the complete thread
const a = await call('capture_event', {
  label_skill: 'supplier-risk',
  instance_page_id: page('order-4500123'),
  title: 'Supplier risk: Meier-Guss downgraded',
  body: 'Meier-Guss was downgraded from A to B. Deliveries of housing GH-4711 slip by 14 days.',
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
  title: 'Kessler delivery risk',
  body: 'Kessler Werkzeugbau: their logistics partner is insolvent. ETA for order 4500131 is unknown.',
  mime: 'text/plain',
  source: 'demo',
});
const csB = await until('the second changeset', () => openChangesetOn(page('order-4500131')));

console.log(
  JSON.stringify({
    rootA: a.event_id,
    rootB: b.event_id,
    promoted: csA.changeset_id,
    awaiting: csB.changeset_id,
  }),
);
