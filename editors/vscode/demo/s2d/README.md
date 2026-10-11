# Source-to-Deliver demo

This demo follows three illustrative decisions: a supplier delay, an outbound load consolidation and a spare-part last-time buy. Start with the [rehearsal](REHEARSAL.md) for the click path and the exact words to use on stage. The [realism review](REALISM-REVIEW.md) records limitations and operational follow-up.

`sync.sh` copies the shared `ops/seed/s2d-demo` data from the hetzner repository into a temporary demo directory and adds view labels with `overlay.py`. The source data and the three incoming mails remain unchanged. `seed.mjs` queries the copied data, then a script creates proposals and run traces through the gateway. The model is scripted; the gateway queries, held changes, review and approval are real. Every figure displayed by a proposal comes from the queried data.

Approval records a decision in Escurel for execution. It does not confirm supplier capacity, reserve stock, place a purchase order, book a carrier or update an ERP or transport system. A planner, shipping coordinator or after-sales owner must complete and confirm those steps separately. The data is synthetic and anonymised; its screens retain the “Illustrative demo data” label.

The last-time-buy warehouse split sums to the recommended quantity (634 = 349 + 158 + 127; largest-remainder allocation, hetzner #929). Approval still only records the decision for execution: inventory control confirms the regional allocation before purchasing places the order.
