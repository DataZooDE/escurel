# Demo of the escurel VS Code extension

```sh
cargo build --release -p escurel-test-support -p escurel-runner   # once
cd editors/vscode && npm ci && npm run build                       # once
demo/run.sh start      # real gateway + real runner + a story already played + a signed-in window
demo/run.sh stop
```

What it starts: a gateway that **verifies tokens** (so the runner can mint a token per run, which is
how a changeset is attached to the run that wrote it), a runner with the echo harness, and a
throwaway VS Code profile (your own settings and extensions are untouched). A bearer file is kept
fresh, so the window stays signed in however long the demo runs. `ESCUREL_DEMO_CDP_PORT=9350`
exposes the window to a debugger for screenshots.

## The state it leaves

| Where         | What you see                                                                                                                                                                                                                                                                          |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Knowledge     | Skills in folders (`sales/orders`, `purchasing/risk`, `purchasing/follow-ups`, `purchasing/suppliers`, `plumbing/…`), each with a role icon: records (cylinder), processes (play), reports (chart), helpers (tools; the plumbing folder starts collapsed). Five orders and a supplier |
| Thread (open) | "Vendor 100234 Meier-Guss: PO 4500087412 confirmation moved +14 days" → run → changeset **promoted** → cascade event → the follow-on's own run                                                                                                                                        |
| Awaiting you  | One changeset, sales order `4500131`, proposed by the agent for "PO 4500087433 confirmed 120 of 200 PC"                                                                                                                                                                               |
| Inbox         | Both signals, newest first                                                                                                                                                                                                                                                            |

## How the Knowledge tree is organised

Every demo skill declares `folder:`, `role:` and `tags:` in its frontmatter (the vocabulary follows Google's
Open Knowledge Format). The tree nests skills under their folder, sorts folders first and then skills by
role (record, process, report, helper) and name, and starts folders that hold only helpers collapsed. Open
`plumbing > sap > order-lines > all`: it is a read-only SQL view over a JSON extract, so the page shows the
form and, beneath it, a **Source data** table of the rows the source holds, under a `read-only (source)`
badge. A skill without a folder sits at the top level; its role is inferred when it declares none.

## A walkthrough (about ten minutes)

1. **The thread canvas.** Pan by dragging, zoom with the wheel, `Fit`. Click a card: the inspector
   shows what the gateway really said, and the Threads outline follows. Arrow keys walk the graph.
2. **The outline.** Collapse the run's card on the canvas: its row collapses in the Threads view.
3. **Run detail.** Double-click the run card: plan, attempts, and one row per tool call.
4. **Awaiting you → review.** Open the sales order 4500131 changeset (Kessler): a diff per draft (base ↔ proposed).
   Leave a comment on a line. It is stored as an event, not in the editor.
5. **Promote.** Promote the changeset. Watch the open thread of that event grow a cascade event
   and a follow-on run **without a reload**: nothing refreshes it; a live subscription does.
6. **Knowledge.** Open `order-4500123` as a page (read-only form), then `Markdown`: edit and save;
   the edit is held as _your_ draft and shows up in Awaiting, the page itself does not move.
7. **Search.** `Escurel: Search` for "Meier" and open the hit.
8. **The analysis a run leaves behind.** Open Knowledge → `supplier-risk-analysis` → the Meier-Guss
   instance. A supplier-risk run does not only change the order: it persisted what it worked out as
   an instance of its own (risk level and score, orders affected, net value at risk), in the same
   changeset, so one promotion published both. Its body states the chart's takeaway as one sentence
   and carries the table behind it, so a reader (or agent) without Peacock still gets the picture;
   the graph itself is drawn by Peacock from the skill's `viewer:` report. The buttons at the bottom
   are the follow-ups the skill declares (Notify the affected customers, Ask the supplier for a new
   confirmation); the third action (a chat prompt) is Peacock's and is not offered here.
9. **A link in a page.** Open `order-4500131`: its History names the vendor as a link
   (Meier-Guss GmbH). Click it, or Tab to it and press Enter: the supplier opens in its own tab.
10. **Start a skill.** At the bottom of an order, the **Supplier risk for … with an agent** button.
    Its chevron (or the arrow-down key) offers: _Start in background_, _First make a plan_,
    _Start in terminal_, _View skill_. Start one in the background and watch the Runner view (right
    side) show it live, then the thread of that event grow a run.
11. **First make a plan.** Choose it: the runner drafts a plan and stops; a notification offers
    **Approve plan**. Nothing runs until you say so.
12. **Cancel and retry.** In the Runner view, open a live run and **Cancel run**. Right-click a run
    under _Dead letters_: **Retry run** asks the runner again and tells you what happened. Requeue,
    Pause and Resume are there too, deactivated with the reason, because they are for admins.

## Limits worth saying out loud

- The runner is the echo harness: it folds the signal into the page, it does not reason. The
  lineage, the live updates and the review are real; the "agent" is a stand-in.
- Sign-in is a test token, kept fresh from a file by `demo/bootstrap`, which is not part of the
  shipped extension. A real install signs in with OIDC.
- `customer-order` deliberately has no `cascade:` routing: a cascade from an order back to the
  supplier skill would loop on the same page (each fold triggers the next), which a stand-in agent
  that always writes cannot break out of.
- "Start in terminal" needs `escurel.shellHarness` set in your own user settings (it is never read
  from a workspace); the demo profile leaves it empty, so there it only explains how to enable it.
- The analysis chart is drawn by **Peacock**, not by this extension (which shows the page's markdown and
  its table). For Peacock to have rows, the demo ships a read-only SQL view over the demo's order lines:
  `sources/order-lines/*.json` (one JSON file per order item) behind the `order-lines` skill, and the
  authored query page `query::analysis_orders` that the report's `data:` reads. `demo/run.sh` points the
  skill at the absolute path (DuckDB resolves a relative glob against the server's cwd) and
  `demo/materialise.mjs` creates the `order-lines::all` instance as the admin, which a sql_view
  requires. The query finds an analysis's orders by its id prefix (`meier-guss-…` takes the lines whose
  supplier is `meier-guss`), so the id scheme must keep the supplier slug first. Checked for real:
  `peacock author preview supplier-risk-report.md` against this demo renders 2 rows, 1 chart, rasterized.
