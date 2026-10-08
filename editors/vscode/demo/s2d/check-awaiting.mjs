// Prints what waits for a planner in the gateway now (open changesets) and whether the three S2D proposals are
// among them. A warning, never a failure: it is for a person (and the logs) to see that the stage starts with all
// three proposals open and nothing promoted.
//
//   node check-awaiting.mjs <gateway.json> <bearer.json>
import { readFileSync } from 'node:fs';

const [gatewayFile, bearerFile] = process.argv.slice(2);
const gw = JSON.parse(readFileSync(gatewayFile, 'utf8').split('\n')[0]);
const { bearer } = JSON.parse(readFileSync(bearerFile, 'utf8'));
const res = await fetch(`${gw.gateway_url}/mcp`, {
  method: 'POST',
  headers: { 'content-type': 'application/json', authorization: `Bearer ${bearer}` },
  body: JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'tools/call',
    params: { name: 'list_changesets', arguments: {} },
  }),
});
const out = (await res.json()).result?.structuredContent ?? {};
const open = (out.changesets ?? []).filter((c) => c.status === 'open');
const targets = open.flatMap((c) => c.target_page_ids ?? []).map((p) => p.split('/').pop().replace(/\.md$/, ''));
const wanted = ['res-l-24117', 'tp-stuttgart-lyon-fr-2026-10-08', 'ltb-sp-3307'];
const missing = wanted.filter((w) => !targets.includes(w));
console.log(`awaiting you: ${open.length} open proposal(s): ${targets.join(', ') || 'none'}`);
console.log(
  missing.length === 0
    ? 'the three S2D proposals are open (nothing promoted)'
    : `WARNING: not open: ${missing.join(', ')}`,
);
