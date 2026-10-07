// Loads the S2D (Source-to-Deliver) demo into a running demo gateway and plays its three stories up to
// the point where a planner has to decide. Everything goes through the gateway's MCP surface, as a
// real client would; nothing is written to its files.
//
//   node seed.mjs <gateway.json> <bearer.json> <built-by-sync.sh dir>
//
// What is REAL: the pages and parquet from the hetzner seed, the queries (run live against DuckDB), the
// gateway's autonomy gate (the proposals are HELD drafts of a run-bound agent token, so promoting
// one in Awaiting you is the approval), the run records and the tool-call trace.
// What is SCRIPTED: the model. No LLM is in the demo, so this script plays the agent: it makes the
// same calls the agent would (queries, a read) and writes the proposal. See REHEARSAL.md.
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const [gatewayFile, bearerFile, builtDir] = process.argv.slice(2);
const gw = JSON.parse(readFileSync(gatewayFile, 'utf8').split('\n')[0]);
const user = JSON.parse(readFileSync(bearerFile, 'utf8')).bearer;
const admin = gw.admin_bearer;

async function call(token, name, args) {
  const res = await fetch(`${gw.gateway_url}/mcp`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
  const out = body.result.structuredContent ?? {};
  if (body.result.isError || out.ok === false) throw new Error(`${name}: ${JSON.stringify(out)}`);
  return out;
}

const SKILLS = (id) => `markdown/skills/${id}.md`;
const INSTANCE = (skill, id) => `markdown/instances/${skill}/${id}.md`;
const read = (p) => readFileSync(p, 'utf8');
const files = (dir) => readdirSync(join(builtDir, 'pages', dir)).filter((f) => f.endsWith('.md'));

// 1. The pages: skills (data views, records), reports, queries.
// The seed grows on the hetzner side (new queries and reports): the pages the stories rely on must load,
// anything else that does not is warned about and left out, never a reason to lose the whole demo.
const CORE = new Set(['exception_exposure', 'sourcing_options', 'outbound_transports', 'spare_parts', 'supplier_exception',
  'exception_resolution', 'transport_plan', 'ltb_decision', 'exception-impact-report', 'resolution-options-report',
  'consolidation-plan-report', 'ltb-report', 'delay_impact', 'delay_impact_summary', 'resolution_options',
  'consolidation_plan', 'consolidation_candidates', 'ltb_parts', 'ltb_whatif', 'ltb_profile', 'ltb_warehouse_split', 'ltb_quantity']);
async function load(pageId, content, id) {
  try {
    await call(admin, 'update_page', { page_id: pageId, content });
  } catch (e) {
    if (CORE.has(id)) throw e;
    console.error(`s2d: WARNING skipped ${pageId}: ${String(e.message).slice(0, 200)}`);
  }
}
for (const f of files('skills')) await load(SKILLS(f.slice(0, -3)), read(join(builtDir, 'pages/skills', f)), f.slice(0, -3));
for (const f of files('reports')) await load(SKILLS(f.slice(0, -3)), read(join(builtDir, 'pages/reports', f)), f.slice(0, -3));
for (const f of files('queries')) await load(INSTANCE('query', f.slice(0, -3)), read(join(builtDir, 'pages/queries', f)), f.slice(0, -3));
for (const skill of ['exception_exposure', 'sourcing_options', 'outbound_transports', 'spare_parts']) {
  await call(admin, 'create_sql_instance', { skill, id: 'all', overlay_body: `# ${skill}\nIllustrative demo data (synthetic).` });
}

const rowsOf = (r) => r.rows ?? r.result ?? r.data ?? [];
const q = async (token, ref, params) => rowsOf(await call(token, 'query_instance', { ref, params }));
// The optimizer queries need the anofox_optimize extension in the gateway; without it they are absent
// or fail, and the stories fall back to the plain options. Never a reason to lose the demo.
const tryQ = async (token, ref, params) => {
  try {
    const rows = await q(token, ref, params);
    return rows.length ? rows : null;
  } catch {
    return null;
  }
};

// 2. The oracle: the facts the rehearsal script relies on, asserted against THIS gateway.
function expect(what, ok, got) {
  if (!ok) throw new Error(`S2D check failed: ${what}: ${JSON.stringify(got)}`);
}
const DELAY = { supplier: 'baltic-components', lot: 'L-24117', delay_days: 21 };
{
  const s = (await q(user, 'delay_impact_summary', DELAY))[0];
  expect('4 of the orders go late for 1,160 units', Number(s.orders_late) === 4 && Number(s.units_on_late_orders) === 1160, s);
  const o = await q(user, 'delay_impact', DELAY);
  expect('12 orders are fed by the lot', o.length === 12, o.length);
  const opts = await q(user, 'resolution_options', { material: 'CB-7', delay_days: 21, qty_needed: 1160 });
  expect('five recovery options, doing nothing last', opts.length === 5 && opts.at(-1).option_type === 'do_nothing', opts);
  const plan = await q(user, 'consolidation_plan', { lane: 'Stuttgart -> Lyon (FR)', ship_on: '2026-10-08' });
  const ship = plan.filter((x) => x.decision === 'consolidate').map((x) => x.shipment_id);
  expect('SH-77001..3 consolidate', JSON.stringify(ship) === '["SH-77001","SH-77002","SH-77003"]', ship);
  // The inbound groupage lane: 18 pallets consolidate on 2026-10-13 (IN-55101 and IN-55102).
  const inbound = await tryQ(user, 'consolidation_plan', { lane: 'Gdansk -> Stuttgart (inbound groupage)', ship_on: '2026-10-13' });
  if (inbound) {
    const pallets = inbound.filter((x) => x.decision === 'consolidate').reduce((n, x) => n + Number(x.pallets), 0);
    expect('18 inbound pallets consolidate', pallets === 18, inbound);
  } else console.error('s2d: WARNING the inbound lane is not in this version of the seed');
  const rec = await tryQ(user, 'recovery_plan', { material: 'CB-7', delay_days: 21, qty_needed: 1160 });
  if (rec) {
    const picked = rec.filter((x) => x.chosen && Number(x.use_qty) > 0).map((x) => `${x.option_id}:${x.use_qty}`).sort();
    expect('the optimizer picks the 250 reallocation and the 910 expedite', JSON.stringify(picked) === '["partial-expedite:910","realloc-wh-mid:250"]', picked);
    expect('at EUR 3,150', rec.reduce((n, x) => n + Number(x.cost_eur), 0) === 3150, rec);
  } else console.error('s2d: WARNING the optimizer pages are not loaded (no anofox_optimize extension): the stories use the plain options');
  const trucks = await tryQ(user, 'consolidation_trucks', { lane: 'Stuttgart -> Lyon (FR)', ship_on: '2026-10-08' });
  if (trucks) expect('one truck and EUR 1,140 saved', Number(trucks[0].trucks_needed) === 1 && Number(trucks[0].lane_saving_eur) === 1140, trucks);
  const w640 = (await q(user, 'ltb_whatif', { part: 'SP-3307', qty: 640 }))[0];
  const w400 = (await q(user, 'ltb_whatif', { part: 'SP-3307', qty: 400 }))[0];
  expect('640 covers the lifetime, 400 does not', Number(w640.probability_covers_lifetime) >= 0.94 && Number(w400.probability_covers_lifetime) < 0.2, [w640, w400]);
}

// 3. The stories.
const cell = (v) => String(v ?? '').replaceAll('|', '/');
const table = (rows, cols) =>
  [`| ${cols.map(([, h]) => h).join(' | ')} |`, `|${cols.map(() => '---').join('|')}|`, ...rows.map((r) => `| ${cols.map(([k]) => cell(r[k])).join(' | ')} |`)].join('\n');

const frontmatter = (kind, skill, id, fields) =>
  `---\nkind: ${kind}\nskill: ${skill}\nid: ${id}\n${Object.entries(fields)
    .map(([k, v]) => `${k}: ${Array.isArray(v) ? `[${v.join(', ')}]` : typeof v === 'string' ? JSON.stringify(v) : v}`)
    .join('\n')}\n---\n`;
const NOTE = '_Illustrative demo data (synthetic): not customer data, and the figures are not customer results._';

async function mail(label, title, body) {
  return call(user, 'capture_event', { label_skill: label, title, body, mime: 'text/plain', source: 'demo-mail' });
}

// A run of an agent that is NOT the runner's: the gateway mints a run-bound token (a "workbench" run),
// so what it writes is attributed to the run and a held write shows under it in the thread.
async function agentRun(skill, event, targetPage, work) {
  const run = await call(user, 'mint_agent_token', { skill, root_event_id: event.event_id, target_page_id: targetPage, ttl_secs: 14400 });
  const agent = run.token;
  const summary = await work(agent);
  await call(admin, 'capture_event', {
    event_id: `run:${run.run_id}:finished`,
    source: 'escurel-runner',
    label_skill: 'escurel:run',
    kind: 'system',
    instance_page_id: targetPage,
    title: 'run-finished',
    mime: 'application/json',
    body: JSON.stringify({ status: 'processed', attempts: 1, held: true, summary, tool_calls: 0, produced_instance: targetPage, plan: null, usage: null }),
    provenance: { runner: { run_id: run.run_id, root_event_id: event.event_id, agent: `agent:${skill}`, harness: 'workbench', attempt: 1 } },
  });
  return run;
}

// 3a. Source: a supplier mail, 21 days late.
const MAIL = `Subject: Delivery delay PO-4500182 / lot L-24117

Dear planning team, due to a capacity bottleneck on our SMT line, lot L-24117 (controller board CB-7, 2,400 pcs) for PO-4500182 will ship about three weeks later than confirmed. We will confirm the new ship date shortly. Kind regards, Baltic Components, order desk`;
const mailEv = await mail('supplier_exception', 'Delivery delay PO-4500182 / lot L-24117', MAIL);
const excPage = INSTANCE('supplier_exception', 'l-24117');
const resPage = INSTANCE('exception_resolution', 'res-l-24117');
const exceptionDoc = (status) =>
  frontmatter('instance', 'supplier_exception', 'l-24117', {
    supplier: 'baltic-components', lot: 'L-24117', material: 'CB-7', delay_days: 21, status,
    received_on: '2026-10-07', reason: 'Capacity bottleneck on the SMT line',
  }) + `# Delay on lot L-24117\n\n${NOTE}\n\n## Message\n\n${MAIL.split('\n').map((l) => `> ${l}`).join('\n')}\n`;
await call(user, 'update_page', { page_id: excPage, content: exceptionDoc('open') });
const excHead = await call(user, 'expand', { page_id: excPage });

await agentRun('supplier_exception', mailEv, resPage, async (agent) => {
  const summaryRow = (await q(agent, 'delay_impact_summary', DELAY))[0];
  const orders = await q(agent, 'delay_impact', DELAY);
  await q(agent, 'resolution_options', { material: 'CB-7', delay_days: 21, qty_needed: 1160 });
  const late = orders.filter((o) => o.status === 'late');
  const penalty = Number(summaryRow.penalty_exposure_eur);
  // The cheapest combination that covers every late unit, solved exactly by the optimizer when it is loaded.
  const rec = await tryQ(agent, 'recovery_plan', { material: 'CB-7', delay_days: 21, qty_needed: 1160 });
  const chosen = rec?.filter((x) => x.chosen && Number(x.use_qty) > 0);
  const recommended = chosen
    ? chosen.map((x) => `${x.source} ${x.use_qty}`).join(' + ')
    : 'Reallocate 250 from the Central Europe warehouse, expedite 910 from the supplier';
  const estCost = chosen ? chosen.reduce((n, x) => n + Number(x.cost_eur), 0) : 3150;
  const proposalTable = chosen
    ? table(chosen.map((x) => ({ ...x, lead: `${x.lead_days} days`, cost: `EUR ${Number(x.cost_eur).toLocaleString('en-US')}` })),
        [['source', 'Option'], ['use_qty', 'Units'], ['lead', 'Lead time'], ['cost', 'Cost'], ['risk', 'Risk']])
    : '| Option | Units | Lead time | Cost | Risk |\n|---|---|---|---|---|\n| Reallocate from the Central Europe warehouse | 250 | 2 days | EUR 1,050 | low |\n| Partial expedite at the supplier | 910 | 8 days | EUR 2,100 | medium |';
  const method = chosen ? 'Chosen by an exact optimisation (the cheapest combination that covers every late unit). ' : '';
  const resolution =
    frontmatter('instance', 'exception_resolution', 'res-l-24117', {
      status: 'approved', exception: 'l-24117', supplier: 'baltic-components', lot: 'L-24117', material: 'CB-7',
      delay_days: 21, orders_late: late.length, penalty_exposure_eur: penalty, qty_needed: 1160,
      recommended_option: recommended, est_cost_eur: estCost, risk: 'medium',
    }) +
    `# Resolution for lot L-24117\n\n${NOTE}\n\n## Situation\n\nLot L-24117 (controller board CB-7) arrives ${DELAY.delay_days} days late. Of the ${orders.length} customer orders it feeds, ${late.length} go late (${late.map((o) => `${o.days_late} days`).join(', ')}); ${orders.length - late.length} absorb the delay.\n\n## Proposal\n\n${proposalTable}\n\n${method}Together 1,160 units: every late order is covered. Estimated cost EUR ${estCost.toLocaleString('en-US')} against a penalty exposure of EUR ${penalty.toLocaleString('en-US')}.\n\n## Changes on approval\n\n- Split the purchase order: 910 units expedited.\n- Transfer 250 units from the Central Europe warehouse.\n- Keep the promised delivery dates of the late orders.\n- The expedited part can join the inbound groupage from Gdansk to Stuttgart on 2026-10-13 (18 of 33 pallets are booked).\n\nApproving records the decision for execution. Nothing in the planning or ERP system is changed by this page.\n`;
  // ONE changeset holds both pages, so approving it records the decision AND resolves the exception.
  const first = await call(agent, 'create_draft', { target_page_id: resPage, content: resolution, new_changeset: true, event_id: mailEv.event_id });
  await call(agent, 'create_draft', { target_page_id: excPage, content: exceptionDoc('resolved'), base_sha256: excHead.content_sha256, changeset_id: first.changeset_id ?? first.draft?.changeset_id, event_id: mailEv.event_id });
  return `Read the supplier message, projected the impact (${late.length} of ${orders.length} orders late) and proposed a resolution.`;
});

// 3b. Deliver: part loads that can ship together.
const tpMail = await mail('transport_plan', 'Weekly outbound review: part loads on the Stuttgart lanes',
  'Which part-load shipments can we consolidate this week, without breaking a delivery duty or the shelf space at the destination?');
const tpPage = INSTANCE('transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08');
await agentRun('transport_plan', tpMail, tpPage, async (agent) => {
  await q(agent, 'consolidation_candidates', {});
  const plan = await q(agent, 'consolidation_plan', { lane: 'Stuttgart -> Lyon (FR)', ship_on: '2026-10-08' });
  const together = plan.filter((x) => x.decision === 'consolidate');
  const kept = plan.filter((x) => x.decision !== 'consolidate');
  const pallets = together.reduce((n, x) => n + Number(x.pallets), 0);
  // Packed into trucks by the optimizer, when it is loaded.
  const trucks = await tryQ(agent, 'consolidation_trucks', { lane: 'Stuttgart -> Lyon (FR)', ship_on: '2026-10-08' });
  const saving = Number(trucks?.[0]?.lane_saving_eur ?? together[0]?.lane_saving_eur ?? 0);
  const held = Number(together[0]?.held_pallets_total ?? 0);
  const planCols = [['shipment_id', 'Shipment'], ['customer', 'Customer'], ['planned_ship_date', 'Planned'], ['ship_on', 'Ships together on'], ['delivery_duty_date', 'Delivery duty'], ['pallets', 'Pallets']];
  const doc =
    frontmatter('instance', 'transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08', {
      status: 'approved', lane: 'Stuttgart -> Lyon (FR)', ship_on: '2026-10-08', shipments: together.map((x) => x.shipment_id),
      pallets, held_pallets: held, saving_eur: saving,
    }) +
    `# Consolidation: Stuttgart to Lyon, Thursday 2026-10-08\n\n${NOTE}\n\n## Plan\n\n${table(together, planCols)}\n\n## Not consolidated\n\n${kept.map((x) => `- ${x.shipment_id} (${x.customer}): ${x.decision}`).join('\n')}\n\n## Checks\n\n- Every delivery duty of the consolidated shipments is met.\n${trucks ? `- Packed by an optimiser into ${trucks[0].trucks_needed} truck(s): EUR ${saving.toLocaleString('en-US')} saved against sending them separately.\n` : ''}- The ${held} held pallets fit the ${together[0]?.free_pallet_slots ?? held} free shelf slots at the destination.\n\nApproving records the plan for execution; the carrier booking is a separate step.\n`;
  await call(agent, 'create_draft', { target_page_id: tpPage, content: doc, new_changeset: true, event_id: tpMail.event_id });
  return `Checked ${plan.length} shipments on the lane and proposed shipping ${together.length} together.`;
});

// 3c. After-sales: how much to buy before production of a spare part ends.
const ltbMail = await mail('ltb_decision', 'Spare parts reaching end of production',
  'Which spare parts reach end of production, and how many of SP-3307 should we buy to keep the service level?');
const ltbPage = INSTANCE('ltb_decision', 'ltb-sp-3307');
await agentRun('ltb_decision', ltbMail, ltbPage, async (agent) => {
  const parts = await q(agent, 'ltb_parts', {});
  const w640 = (await q(agent, 'ltb_whatif', { part: 'SP-3307', qty: 640 }))[0];
  const w400 = (await q(agent, 'ltb_whatif', { part: 'SP-3307', qty: 400 }))[0];
  const split = await q(agent, 'ltb_warehouse_split', { part: 'SP-3307', qty: 640 });
  const doc =
    frontmatter('instance', 'ltb_decision', 'ltb-sp-3307', {
      status: 'approved', part: 'SP-3307', qty: 640, service_level: 0.95,
      probability_covers_lifetime: Number(Number(w640.probability_covers_lifetime).toFixed(3)),
      expected_runout_year: String(w640.expected_runout_year), stock_value_eur: 755200,
    }) +
    `# Last-time-buy: SP-3307 (servo drive module)\n\n${NOTE}\n\n## Recommendation\n\nBuy 640 units: they hold the 95% service level to the end of service.\n\n## Alternatives considered\n\n- 400 units: about ${(Number(w400.probability_covers_lifetime) * 100).toFixed(0)}% chance to last; expected run-out ${w400.expected_runout_year}.\n- 640 units: about ${(Number(w640.probability_covers_lifetime) * 100).toFixed(0)}% chance to last.\n\n## Warehouse split\n\n${table(split, Object.keys(split[0] ?? {}).map((k) => [k, k.replaceAll('_', ' ')]))}\n\nApproving records the decision for execution; the purchase order is a separate step.\n`;
  await call(agent, 'create_draft', { target_page_id: ltbPage, content: doc, new_changeset: true, event_id: ltbMail.event_id });
  return `Compared ${parts.length} parts reaching end of production and proposed a last-time-buy for SP-3307.`;
});

console.log(JSON.stringify({ exception: mailEv.event_id, transport: tpMail.event_id, ltb: ltbMail.event_id }));
