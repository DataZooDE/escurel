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

| Where         | What you see                                                                                                                                   |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Knowledge     | Skills `supplier-risk`, `customer-order`, `supplier`; five orders and a supplier                                                               |
| Thread (open) | "Vendor 100234 Meier-Guss: PO 4500087412 confirmation moved +14 days" → run → changeset **promoted** → cascade event → the follow-on's own run |
| Awaiting you  | One changeset, sales order `4500131`, proposed by the agent for "PO 4500087433 confirmed 120 of 200 PC"                                        |
| Inbox         | Both signals, newest first                                                                                                                     |

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
8. **A link in a page.** Open `order-4500131`: its History names the vendor as a link
   (Meier-Guss GmbH). Click it, or Tab to it and press Enter: the supplier opens in its own tab.
9. **Start a skill.** At the bottom of an order, the **Supplier risk for … with an agent** button.
   Its chevron (or the arrow-down key) offers: _Start in background_, _First make a plan_,
   _Start in terminal_, _View skill_. Start one in the background and watch the Runner view (right
   side) show it live, then the thread of that event grow a run.
10. **First make a plan.** Choose it: the runner drafts a plan and stops; a notification offers
    **Approve plan**. Nothing runs until you say so.
11. **Cancel and retry.** In the Runner view, open a live run and **Cancel run**. Right-click a run
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
