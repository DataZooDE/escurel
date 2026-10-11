# Source-to-Deliver realism review

## Scope and source of truth

The shared hetzner `ops/seed/s2d-demo` remains the single source for data, identifiers and the three incoming mails. [`sync.sh`](sync.sh) copies it; [`overlay.py`](overlay.py) adds extension labels; [`seed.mjs`](seed.mjs) stages proposals. This review concerns the experience an executive, planner, shipping coordinator or after-sales manager sees in the VS Code demo. All data is illustrative, synthetic and anonymised.

## Findings and changes made

- **Approval sounded like fulfilment.** The supplier resolution promised to keep delivery dates, and the transport plan asserted every delivery duty was met before carrier and load readiness were confirmed. [`seed.mjs`](seed.mjs) now presents those as checks and names planning, purchasing, inventory control and shipping as owners of the next steps. It says explicitly that approval records a decision for execution, without placing a booking or purchase order.
- **The last-time-buy percentage sounded like a delivery SLA.** The queried probability estimates whether stock lasts through the service horizon; it is not an order fill rate or delivery guarantee. [`seed.mjs`](seed.mjs) and [`REHEARSAL.md`](REHEARSAL.md) now call it modelled stock coverage and point to the supplier cut-off and operational purchase order.
- **The Overview tile called all records “Open items.”** It counts records by skill, regardless of their status. [`src/overview/model.ts`](../../src/overview/model.ts) now calls it “Records by area,” and its headline counts record areas.
- **The demonstration could blur scripted work with live work.** [`README.md`](README.md) and [`REHEARSAL.md`](REHEARSAL.md) now give an explicit stage disclosure: data queries and the approval gate run live; a script writes the agent proposals and run traces. The supplier and carrier mail text was left unchanged.
- **An anonymised talk track named a specific manufacturer.** [`REHEARSAL.md`](REHEARSAL.md) now uses a generic manufacturer. Technical record IDs remain in click paths so the presenter can find the right record, but they should be read as navigation aids, not business language.

## Source-data issue: RESOLVED (hetzner #929)

The shared snapshot's last-time-buy recommendation was 634 units while the displayed regional allocation was 349 + 159 + 127 = 635: `ltb_warehouse_split` rounded each warehouse separately. Fixed upstream in hetzner-agent-substrate #929 (largest-remainder allocation): the split is now Central Europe 349, Americas 158, Asia-Pacific 127 = 634; the stock value is unchanged at EUR 748,120. The demo reads the split from the data, so a re-sync picks it up; [`REHEARSAL.md`](REHEARSAL.md) quotes the new figures. Approval still only records the decision for execution.

## Recommendations outside this extension and demo layer

- Add an explicit approver and hand-off owner per decision, with a recorded approval time, reason, source-data version and acknowledgement from the team that actually executes it. The current thread and trace show the proposal and approval, but not a confirmed ERP or carrier action.
- For the supplier exception, confirm Incoterms, the supplier's revised ship date, partial-delivery acceptance, transport route, stock availability and customer notification rules. The mail does not specify these, and its text is frozen. Treat the penalty amount as modelled exposure until contract terms and liability are checked.
- For consolidation, capture the carrier quote, booking reference, lane cut-off time zone, load readiness, weight and dimensions, warehouse receiving capacity, delivery appointment and proof of delivery. Planned arrival and estimated saving are not a service commitment.
- For last-time buy, reconcile the regional split, test demand and obsolescence assumptions, storage and working-capital limits, supplier minimum order and last-order date, then assign a purchasing owner. The modelled probability alone cannot settle those trade-offs.
- Give planner, controller, shipping and after-sales roles their own queues and notifications. Approval should trigger a tracked operational task; completion should be acknowledged with evidence and surfaced as a distinct state.

## Verification limits

The local unit tests cover labels, manifest wiring and proposal wording. The component runner could not bind a local server in this sandbox, and Docker was inaccessible for visual baselines. Live S2D end-to-end verification needs Xvfb, which is unavailable here, so it was not run. The shared source snapshot was not edited.
