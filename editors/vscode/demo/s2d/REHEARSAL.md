# Source-to-Deliver rehearsal (VS Code demo)

Three agent proposals wait for a planner in **Awaiting you**: a late supplier lot, part loads that can
ship together, and a last-time buy. Everything on screen is **illustrative demo data** (synthetic, one
anonymised industrial manufacturer): not customer data, and no figure is a customer result.

Start it: `editors/vscode/demo/run.sh start` (the S2D data is on by default; `ESCUREL_DEMO_S2D=0`
leaves it out). The two optimizer pages (cheapest recovery plan, truck packing) need the local
`anofox_optimize` DuckDB build at `~/Projects/datazoo/anofox-optimize/build/release/extension/anofox_optimize/`
(`ESCUREL_DEMO_OPTIMIZE_EXT` overrides the path). It was built for DuckDB v1.5.5, so `run.sh` starts the
gateway against the pinned libduckdb the build downloaded (`target/duckdb-download/…/1.5.5`); if the
versions still differ it says so, leaves those two pages out and the stories use the plain options. The line `s2d: built from hetzner-agent-substrate <sha>` names the version of the
shared seed it was built from. Reset between rehearsals: `run.sh start` again.

## What is real and what is scripted

| Real | Scripted |
|---|---|
| The data, skills, queries and reports (the hetzner seed), run live against DuckDB | The model: no LLM is in the demo. A script plays the agent: it makes the same reads (queries, one page) and writes the proposal text |
| The gateway's human gate: the agent's write is HELD, so promoting it IS the approval | The supplier mail arrives as an inbox event; a person did not type it |
| Who proposed what: each proposal belongs to its own run, with its tool-call trace | The run records are written by the script, not by an agent harness |
| The impact, option and plan tables (computed from the data when the page opens) | |

Say it plainly on stage: *"This is what the next step looks like. The parts run at customers today; here
they are assembled in one workbench."* Never say a system of record was updated: **approving records the
decision for execution**; the purchase-order split, the stock transfer, the carrier booking are separate
steps.

## Story 1: a supplier lot is three weeks late (Source)

1. **Inbox**: "Delivery delay PO-4500182 / lot L-24117" (the supplier's mail). Open it: a capacity
   bottleneck at the supplier, lot L-24117 (controller board CB-7, 2,400 pieces), about three weeks late.
2. **Knowledge** > logistics > source > Supplier exception > **l-24117**: the exception record. Under the
   fields, **Impact of a supplier delay**: **4 orders late**, **132,400 penalty exposure (EUR)**, and
   the table of the 12 orders the lot feeds, worst first: 18, 13, 9 and 4 days late; the other 8 absorb
   the delay. (Say: "12 orders depend on this lot, 4 of them go late.")
3. **Awaiting you** > "res-l-24117 +1 — Proposed changes": two pages in ONE proposal: the resolution and the
   exception. Open it to review the diff: the proposal table is two options, **250 units from the Central
   Europe warehouse (2 days, low risk, EUR 1,050)** and **740 units expedited at the supplier (8 days,
   medium risk, EUR 2,100)**: together the 990 units on late orders, EUR 3,150 against 132,400. With
   the optimizer loaded the text says the combination was chosen by an exact optimisation (the cheapest
   that covers every late unit). **Preview the proposal** (the first button of the review editor) shows it
   as a formatted page next to the source. The expedited part can join the inbound groupage Gdansk to
   Stuttgart on 2026-10-12 (18 pallets consolidate).
4. **Approve** (the check mark on the Awaiting row). "Applied 2 changes": the resolution is recorded as
   approved and the exception shows **resolved**, together. The open record updates at once.
5. Optional: the thread behind it (Open thread / Open run): the mail, the run of the agent, what it read.

## Story 2: part loads that can ship together (Deliver)

1. Inbox: the carrier mail "Booking cut-off week 41, Stuttgart outbound" (the same mail the chat demos use: confirm part-load bookings by 14:00; full-truck capacity Stuttgart -> Lyon on Thursday 08 Oct).
2. **Awaiting you** > "tp-stuttgart-lyon-fr-2026-10-08": approve it.
   Before approving, **Preview the proposal** on the draft shows the plan table.
3. Knowledge > logistics > deliver > Transport plan > the plan: **Consolidation plan**, one card per shipment
   on a narrow window (a table on a wide one); what is the same on every row is said once, above the rows:
   ships together on 2026-10-08, **arrives 2026-10-10**, so each card's delivery duty reads against one arrival date.
   **SH-77001, SH-77002 and SH-77003 ship together on Thursday 2026-10-08** (23 pallets; 14 held pallets fit the
   14 free slots; EUR 1,140 saved; with the optimizer: **packed into 1 truck**). **SH-77004 stays**: holding it would miss its delivery duty of 2026-10-09.
   **SH-77005 is not ready** before 2026-10-12.

## Story 3: how many to buy before production ends (After-sales)

1. Inbox: the supplier notice "Product discontinuation notice: servo drive module SD-40" (SP-3307; last production date 31 December 2026; place last-time-buy orders before the production stop).
2. **Awaiting you** > "ltb-sp-3307": approve it.
3. Knowledge > logistics > after-sales > Last-time-buy decision > the record. **Probability it lasts: 95%** (0.949).
   Buying **634 units** holds the 95% service level to end of service (expected lifetime demand 509);
   the table shows the stock value (EUR 748,120) and the **warehouse split: EMEA 349, Americas 159,
   APAC 127**. The alternative of 400 units is in the proposal's text: about an 8% chance, running out
   in 2032, two years before the end of service in 2034.

## Mapping to the talk (min-by-min, Part 3 and assembly)

| Talk | Screen |
|---|---|
| Cold open: supplier mail to resolution | Story 1, steps 1 to 4 |
| "Every step runs at a real customer" | Story 1 step 5: the thread names which agent did what |
| Part 3: the daily decision | Story 2 |
| "Where Bosch would start": three agents | Story 3, then the three rows in Awaiting you |

## If something goes wrong

* A page shows no figures: the report could not be loaded for that record. The record is complete
  without it; reload with the refresh button in the Knowledge title.
* Awaiting you is empty: the demo was started with `ESCUREL_DEMO_S2D=0`, or a proposal was already
  approved. Restart with `run.sh start`.
* The window looks like an IDE: Focus mode (a separate branch) hides the chrome.
