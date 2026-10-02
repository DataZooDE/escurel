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

| Where         | What you see                                                                                                    |
| ------------- | --------------------------------------------------------------------------------------------------------------- |
| Knowledge     | Skills `supplier-risk`, `customer-order`, `supplier`; five orders and a supplier                                |
| Thread (open) | "Supplier risk: Meier-Guss downgraded" → run → changeset **promoted** → cascade event → the follow-on's own run |
| Awaiting you  | One changeset, `order-4500131`, proposed by the agent for "Kessler delivery risk"                               |
| Inbox         | Both signals, newest first                                                                                      |

## A walkthrough (about ten minutes)

1. **The thread canvas.** Pan by dragging, zoom with the wheel, `Fit`. Click a card: the inspector
   shows what the gateway really said, and the Threads outline follows. Arrow keys walk the graph.
2. **The outline.** Collapse the run's card on the canvas: its row collapses in the Threads view.
3. **Run detail.** Double-click the run card: plan, attempts, and one row per tool call.
4. **Awaiting you → review.** Open the Kessler changeset: a diff per draft (base ↔ proposed).
   Leave a comment on a line. It is stored as an event, not in the editor.
5. **Promote.** Promote the changeset. Watch the open thread of that event grow a cascade event
   and a follow-on run **without a reload**: nothing refreshes it; a live subscription does.
6. **Knowledge.** Open `order-4500123` as a page (read-only form), then `Markdown`: edit and save;
   the edit is held as _your_ draft and shows up in Awaiting, the page itself does not move.
7. **Search.** `Escurel: Search` for "Meier" and open the hit.

## Limits worth saying out loud

- The runner is the echo harness: it folds the signal into the page, it does not reason. The
  lineage, the live updates and the review are real; the "agent" is a stand-in.
- Sign-in is a test token, kept fresh from a file by `demo/bootstrap`, which is not part of the
  shipped extension. A real install signs in with OIDC.
- `customer-order` deliberately has no `cascade:` routing: the echo harness folds the OLDEST inbox
  event that has a target page, so a dead-lettered follow-on would swallow the next run.
