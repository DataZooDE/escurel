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
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name, arguments: args },
    }),
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
const CORE = new Set([
  'exception_exposure',
  'sourcing_options',
  'outbound_transports',
  'spare_parts',
  'supplier_exception',
  'exception_resolution',
  'transport_plan',
  'ltb_decision',
  'exception-impact-report',
  'resolution-options-report',
  'consolidation-plan-report',
  'ltb-report',
  'delay_impact',
  'delay_impact_summary',
  'resolution_options',
  'consolidation_plan',
  'consolidation_candidates',
  'ltb_parts',
  'ltb_whatif',
  'ltb_profile',
  'ltb_warehouse_split',
  'ltb_quantity',
]);
async function load(pageId, content, id) {
  try {
    await call(admin, 'update_page', { page_id: pageId, content });
  } catch (e) {
    if (CORE.has(id)) throw e;
    console.error(`s2d: WARNING skipped ${pageId}: ${String(e.message).slice(0, 200)}`);
  }
}
for (const f of files('skills'))
  await load(SKILLS(f.slice(0, -3)), read(join(builtDir, 'pages/skills', f)), f.slice(0, -3));
for (const f of files('reports'))
  await load(SKILLS(f.slice(0, -3)), read(join(builtDir, 'pages/reports', f)), f.slice(0, -3));
for (const f of files('queries'))
  await load(
    INSTANCE('query', f.slice(0, -3)),
    read(join(builtDir, 'pages/queries', f)),
    f.slice(0, -3),
  );
for (const skill of [
  'exception_exposure',
  'sourcing_options',
  'outbound_transports',
  'spare_parts',
]) {
  await call(admin, 'create_sql_instance', {
    skill,
    id: 'all',
    overlay_body: `# ${skill}\nIllustrative demo data (synthetic).`,
  });
}

const rowsOf = (r) => r.rows ?? r.result ?? r.data ?? [];
const q = async (token, ref, params) =>
  rowsOf(await call(token, 'query_instance', { ref, params }));
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
// The numbers of the rehearsal script come from the shared seed and may move with it. A number that no
// longer matches the script is said loudly but never costs the demo; the stories read the live data.
let drift = 0;
function narrate(what, ok, got) {
  if (ok) return;
  drift += 1;
  console.error(
    `s2d: WARNING the rehearsal script expects ${what}; the data says ${JSON.stringify(got).slice(0, 300)}`,
  );
}
const DELAY = { supplier: 'baltic-components', lot: 'L-24117', delay_days: 21 };
// What the lot's delay puts at risk comes from the data: every quantity below is read from it.
const SUMMARY = (await q(user, 'delay_impact_summary', DELAY))[0];
const UNITS = Number(SUMMARY.units_on_late_orders);
{
  expect(
    'the delay of lot L-24117 makes orders late',
    Number(SUMMARY.orders_late) > 0 && UNITS > 0,
    SUMMARY,
  );
  narrate('4 late orders', Number(SUMMARY.orders_late) === 4, SUMMARY);
  const o = await q(user, 'delay_impact', DELAY);
  narrate('12 orders fed by the lot', o.length === 12, o.length);
  const opts = await q(user, 'resolution_options', {
    material: 'CB-7',
    delay_days: 21,
    qty_needed: UNITS,
  });
  expect(
    'recovery options, doing nothing last',
    opts.length > 1 && opts.at(-1).option_type === 'do_nothing',
    opts,
  );
  const plan = await q(user, 'consolidation_plan', {
    lane: 'Stuttgart -> Lyon (FR)',
    ship_on: '2026-10-08',
  });
  const ship = plan.filter((x) => x.decision === 'consolidate').map((x) => x.shipment_id);
  expect('shipments consolidate on the Lyon lane', ship.length > 0, plan);
  narrate(
    'SH-77001..3 consolidate',
    JSON.stringify(ship) === '["SH-77001","SH-77002","SH-77003"]',
    ship,
  );
  const rec = await tryQ(user, 'recovery_plan', {
    material: 'CB-7',
    delay_days: 21,
    qty_needed: UNITS,
  });
  if (rec) {
    const used = rec.filter((x) => x.chosen).reduce((n, x) => n + Number(x.use_qty), 0);
    expect('the optimizer plan covers every late unit', used === UNITS, rec);
    const picked = rec
      .filter((x) => x.chosen && Number(x.use_qty) > 0)
      .map((x) => x.option_id)
      .sort();
    narrate(
      'the 250 reallocation plus the supplier expedite',
      JSON.stringify(picked) === '["partial-expedite","realloc-wh-mid"]',
      picked,
    );
    narrate(
      'EUR 3,150',
      rec.reduce((n, x) => n + Number(x.cost_eur), 0) === 3150,
      rec.map((x) => x.cost_eur),
    );
  } else
    console.error(
      's2d: WARNING the optimizer pages are not loaded (no anofox_optimize extension): the stories use the plain options',
    );
  const trucks = await tryQ(user, 'consolidation_trucks', {
    lane: 'Stuttgart -> Lyon (FR)',
    ship_on: '2026-10-08',
  });
  if (trucks)
    narrate(
      'one truck and EUR 1,140 saved',
      Number(trucks[0].trucks_needed) === 1 && Number(trucks[0].lane_saving_eur) === 1140,
      trucks,
    );
  const w640 = (await q(user, 'ltb_whatif', { part: 'SP-3307', qty: 640 }))[0];
  const w400 = (await q(user, 'ltb_whatif', { part: 'SP-3307', qty: 400 }))[0];
  narrate(
    '640 covers the lifetime, 400 does not',
    Number(w640.probability_covers_lifetime) >= 0.94 &&
      Number(w400.probability_covers_lifetime) < 0.2,
    [w640, w400],
  );
}
// The inbound groupage lane (Gdansk to Stuttgart): the first date on which its shipments consolidate.
let inbound = null;
for (const ship_on of ['2026-10-12', '2026-10-13']) {
  const rows = await tryQ(user, 'consolidation_plan', {
    lane: 'Gdansk -> Stuttgart (inbound groupage)',
    ship_on,
  });
  if (rows?.some((x) => x.decision === 'consolidate')) {
    inbound = {
      ship_on,
      pallets: rows
        .filter((x) => x.decision === 'consolidate')
        .reduce((n, x) => n + Number(x.pallets), 0),
      booked: rows[0].held_pallets_total ?? '',
    };
    break;
  }
}
if (inbound) narrate('18 inbound pallets consolidate', inbound.pallets === 18, inbound);
else console.error('s2d: WARNING the inbound lane is not in this version of the seed');

// 3. The stories.
const cell = (v) => String(v ?? '').replaceAll('|', '/');
const table = (rows, cols) =>
  [
    `| ${cols.map(([, h]) => h).join(' | ')} |`,
    `|${cols.map(() => '---').join('|')}|`,
    ...rows.map((r) => `| ${cols.map(([k]) => cell(r[k])).join(' | ')} |`),
  ].join('\n');

const frontmatter = (kind, skill, id, fields) =>
  `---\nkind: ${kind}\nskill: ${skill}\nid: ${id}\n${Object.entries(fields)
    .map(
      ([k, v]) =>
        `${k}: ${Array.isArray(v) ? `[${v.join(', ')}]` : typeof v === 'string' ? JSON.stringify(v) : v}`,
    )
    .join('\n')}\n---\n`;
const NOTE =
  '_Illustrative demo data (synthetic): not customer data, and the figures are not customer results._';

async function mail(label, title, body) {
  return call(user, 'capture_event', {
    label_skill: label,
    title,
    body,
    mime: 'text/plain',
    source: 'demo-mail',
  });
}

// A run of an agent that is NOT the runner's: the gateway mints a run-bound token (a "workbench" run),
// so what it writes is attributed to the run and a held write shows under it in the thread.
async function agentRun(skill, event, targetPage, work) {
  const run = await call(user, 'mint_agent_token', {
    skill,
    root_event_id: event.event_id,
    target_page_id: targetPage,
    ttl_secs: 14400,
  });
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
    body: JSON.stringify({
      status: 'processed',
      attempts: 1,
      held: true,
      summary,
      tool_calls: 0,
      produced_instance: targetPage,
      plan: null,
      usage: null,
    }),
    provenance: {
      runner: {
        run_id: run.run_id,
        root_event_id: event.event_id,
        agent: `agent:${skill}`,
        harness: 'workbench',
        attempt: 1,
      },
    },
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
    supplier: 'baltic-components',
    lot: 'L-24117',
    material: 'CB-7',
    delay_days: 21,
    status,
    received_on: '2026-10-07',
    reason: 'Capacity bottleneck on the SMT line',
  }) +
  `# Delay on lot L-24117\n\n${NOTE}\n\n## Message\n\n${MAIL.split('\n')
    .map((l) => `> ${l}`)
    .join('\n')}\n`;
await call(user, 'update_page', { page_id: excPage, content: exceptionDoc('open') });
const excHead = await call(user, 'expand', { page_id: excPage });

await agentRun('supplier_exception', mailEv, resPage, async (agent) => {
  const summaryRow = (await q(agent, 'delay_impact_summary', DELAY))[0];
  const orders = await q(agent, 'delay_impact', DELAY);
  await q(agent, 'resolution_options', { material: 'CB-7', delay_days: 21, qty_needed: UNITS });
  const late = orders.filter((o) => o.status === 'late');
  const penalty = Number(summaryRow.penalty_exposure_eur);
  // The cheapest combination that covers every late unit, solved exactly by the optimizer when it is loaded.
  const rec = await tryQ(agent, 'recovery_plan', {
    material: 'CB-7',
    delay_days: 21,
    qty_needed: UNITS,
  });
  const chosen = rec?.filter((x) => x.chosen && Number(x.use_qty) > 0);
  const recommended = chosen ? chosen.map((x) => `${x.source} ${x.use_qty}`).join(' + ') : '';
  const estCost = chosen ? chosen.reduce((n, x) => n + Number(x.cost_eur), 0) : undefined;
  // Without the optimizer the page lists the plain options and does not pretend to know the cheapest mix.
  const optionsRows = chosen
    ? null
    : await q(agent, 'resolution_options', { material: 'CB-7', delay_days: 21, qty_needed: UNITS });
  const proposalTable = chosen
    ? table(
        chosen.map((x) => ({
          ...x,
          lead: `${x.lead_days} days`,
          cost: `EUR ${Number(x.cost_eur).toLocaleString('en-US')}`,
        })),
        [
          ['source', 'Option'],
          ['use_qty', 'Units'],
          ['lead', 'Lead time'],
          ['cost', 'Cost'],
          ['risk', 'Risk'],
        ],
      )
    : table(
        (optionsRows ?? [])
          .filter((x) => x.option_type !== 'do_nothing')
          .map((x) => ({
            ...x,
            lead: `${x.lead_days} days`,
            cost: `EUR ${Number(x.est_cost_eur).toLocaleString('en-US')}`,
          })),
        [
          ['source', 'Option'],
          ['covered_qty', 'Units it can cover'],
          ['lead', 'Lead time'],
          ['cost', 'Cost'],
          ['risk', 'Risk'],
        ],
      );
  const method = chosen
    ? 'Chosen by an exact optimisation (the cheapest combination that covers every late unit). '
    : 'These are the options on offer; the cheapest combination is not worked out here. ';
  const resolution =
    frontmatter('instance', 'exception_resolution', 'res-l-24117', {
      status: 'approved',
      exception: 'l-24117',
      supplier: 'baltic-components',
      lot: 'L-24117',
      material: 'CB-7',
      delay_days: 21,
      orders_late: late.length,
      penalty_exposure_eur: penalty,
      qty_needed: UNITS,
      ...(chosen ? { recommended_option: recommended, est_cost_eur: estCost } : {}),
      risk: 'medium',
    }) +
    `# Resolution for lot L-24117\n\n${NOTE}\n\n## Situation\n\nLot L-24117 (controller board CB-7) arrives ${DELAY.delay_days} days late. Of the ${orders.length} customer orders it feeds, ${late.length} go late (${late.map((o) => `${o.days_late} days`).join(', ')}); ${orders.length - late.length} absorb the delay.\n\n## Proposal\n\n${proposalTable}\n\n${method}${chosen ? `Together ${UNITS.toLocaleString('en-US')} units: every late order is covered. Estimated cost EUR ${estCost.toLocaleString('en-US')} against` : `${UNITS.toLocaleString('en-US')} units are on late orders, against`} a penalty exposure of EUR ${penalty.toLocaleString('en-US')}.\n\n## Changes on approval\n\n${chosen ? chosen.map((x) => `- ${x.option_type === 'stock_reallocation' ? `Transfer ${x.use_qty} units from ${x.source}.` : `Order ${x.use_qty} units through: ${x.source}.`}`).join('\n') : '- To be decided from the options above.'}\n- Keep the promised delivery dates of the late orders.\n${inbound ? `- The expedited part can join the inbound groupage from Gdansk to Stuttgart on ${inbound.ship_on} (${inbound.pallets} pallets consolidate).\n` : ''}\nApproving records the decision for execution. Nothing in the planning or ERP system is changed by this page.\n`;
  // ONE changeset holds both pages, so approving it records the decision AND resolves the exception.
  const first = await call(agent, 'create_draft', {
    target_page_id: resPage,
    content: resolution,
    new_changeset: true,
    event_id: mailEv.event_id,
  });
  await call(agent, 'create_draft', {
    target_page_id: excPage,
    content: exceptionDoc('resolved'),
    base_sha256: excHead.content_sha256,
    changeset_id: first.changeset_id ?? first.draft?.changeset_id,
    event_id: mailEv.event_id,
  });
  return `Read the supplier message, projected the impact (${late.length} of ${orders.length} orders late) and proposed a resolution.`;
});

// 3b. Deliver: part loads that can ship together.
const tpMail = await mail(
  'transport_plan',
  'Weekly outbound review: part loads on the Stuttgart lanes',
  'Which part-load shipments can we consolidate this week, without breaking a delivery duty or the shelf space at the destination?',
);
const tpPage = INSTANCE('transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08');
await agentRun('transport_plan', tpMail, tpPage, async (agent) => {
  await q(agent, 'consolidation_candidates', {});
  const plan = await q(agent, 'consolidation_plan', {
    lane: 'Stuttgart -> Lyon (FR)',
    ship_on: '2026-10-08',
  });
  const together = plan.filter((x) => x.decision === 'consolidate');
  const kept = plan.filter((x) => x.decision !== 'consolidate');
  const pallets = together.reduce((n, x) => n + Number(x.pallets), 0);
  // Packed into trucks by the optimizer, when it is loaded.
  const trucks = await tryQ(agent, 'consolidation_trucks', {
    lane: 'Stuttgart -> Lyon (FR)',
    ship_on: '2026-10-08',
  });
  const saving = Number(trucks?.[0]?.lane_saving_eur ?? together[0]?.lane_saving_eur ?? 0);
  const held = Number(together[0]?.held_pallets_total ?? 0);
  const planCols = [
    ['shipment_id', 'Shipment'],
    ['customer', 'Customer'],
    ['planned_ship_date', 'Planned'],
    ['ship_on', 'Ships together on'],
    ['delivery_duty_date', 'Delivery duty'],
    ['pallets', 'Pallets'],
  ];
  const doc =
    frontmatter('instance', 'transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08', {
      status: 'approved',
      lane: 'Stuttgart -> Lyon (FR)',
      ship_on: '2026-10-08',
      shipments: together.map((x) => x.shipment_id),
      pallets,
      held_pallets: held,
      saving_eur: saving,
    }) +
    `# Consolidation: Stuttgart to Lyon, Thursday 2026-10-08\n\n${NOTE}\n\n## Plan\n\n${table(together, planCols)}\n\n## Not consolidated\n\n${kept.map((x) => `- ${x.shipment_id} (${x.customer}): ${x.decision}`).join('\n')}\n\n## Checks\n\n- Every delivery duty of the consolidated shipments is met.\n${trucks ? `- Packed by an optimiser into ${trucks[0].trucks_needed} truck(s): EUR ${saving.toLocaleString('en-US')} saved against sending them separately.\n` : ''}- The ${held} held pallets fit the ${together[0]?.free_pallet_slots ?? held} free shelf slots at the destination.\n\nApproving records the plan for execution; the carrier booking is a separate step.\n`;
  await call(agent, 'create_draft', {
    target_page_id: tpPage,
    content: doc,
    new_changeset: true,
    event_id: tpMail.event_id,
  });
  return `Checked ${plan.length} shipments on the lane and proposed shipping ${together.length} together.`;
});

// 3c. After-sales: how much to buy before production of a spare part ends.
const ltbMail = await mail(
  'ltb_decision',
  'Spare parts reaching end of production',
  'Which spare parts reach end of production, and how many of SP-3307 should we buy to keep the service level?',
);
const ltbPage = INSTANCE('ltb_decision', 'ltb-sp-3307');
await agentRun('ltb_decision', ltbMail, ltbPage, async (agent) => {
  const parts = await q(agent, 'ltb_parts', {});
  const w640 = (await q(agent, 'ltb_whatif', { part: 'SP-3307', qty: 640 }))[0];
  const w400 = (await q(agent, 'ltb_whatif', { part: 'SP-3307', qty: 400 }))[0];
  const split = await q(agent, 'ltb_warehouse_split', { part: 'SP-3307', qty: 640 });
  const doc =
    frontmatter('instance', 'ltb_decision', 'ltb-sp-3307', {
      status: 'approved',
      part: 'SP-3307',
      qty: 640,
      service_level: 0.95,
      probability_covers_lifetime: Number(Number(w640.probability_covers_lifetime).toFixed(3)),
      expected_runout_year: String(w640.expected_runout_year),
      stock_value_eur: 755200,
    }) +
    `# Last-time-buy: SP-3307 (servo drive module)\n\n${NOTE}\n\n## Recommendation\n\nBuy 640 units: they hold the 95% service level to the end of service.\n\n## Alternatives considered\n\n- 400 units: about ${(Number(w400.probability_covers_lifetime) * 100).toFixed(0)}% chance to last; expected run-out ${w400.expected_runout_year}.\n- 640 units: about ${(Number(w640.probability_covers_lifetime) * 100).toFixed(0)}% chance to last.\n\n## Warehouse split\n\n${table(
      split,
      Object.keys(split[0] ?? {}).map((k) => [k, k.replaceAll('_', ' ')]),
    )}\n\nApproving records the decision for execution; the purchase order is a separate step.\n`;
  await call(agent, 'create_draft', {
    target_page_id: ltbPage,
    content: doc,
    new_changeset: true,
    event_id: ltbMail.event_id,
  });
  return `Compared ${parts.length} parts reaching end of production and proposed a last-time-buy for SP-3307.`;
});

if (drift)
  console.error(
    `s2d: ${drift} figure(s) differ from the rehearsal script: see the warnings above and update REHEARSAL.md`,
  );
console.log(
  JSON.stringify({ exception: mailEv.event_id, transport: tpMail.event_id, ltb: ltbMail.event_id }),
);
