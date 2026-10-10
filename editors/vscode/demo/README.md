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

The window opens in the **calm focus view**: no menu bar, command center or status bar, the Escurel Calm
theme, only Escurel's icon in the activity bar, and the **Overview** board as the first screen.
`ESCUREL_DEMO_FOCUS=0 demo/run.sh start` keeps the classic IDE look (what the end-to-end tests of the
individual views run in). "Leave focus view" on the board (or the palette) switches back.

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

A document is the third kind of source. `purchasing > documents > supplier-document` holds the frame
agreement the demo uploads as a file when it starts (`/ingest/upload`): its text is chunked, the page
shows the first chunks read-only, and **Open original** saves the uploaded file. Markdown is never
handed to an application, so you get a note that says so and the file is revealed in your file manager,
saved as plain text. A PDF or a plain-text file would open in the system application, and a Word
document asks first.

## A walkthrough (about ten minutes)

0. **The overview.** The window opens on _Today_: decisions waiting (the sales order 4500131 changeset),
   agent activity, anything that failed, open records per kind of work, what just finished. Click a
   line to open what it names, or a tile's title for the whole view.
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
9. **A document.** Open Knowledge → `supplier-document` → the frame agreement. You see its first
   chunks (the delivery-terms clause says a move of more than 7 days is a supply risk: the same fact
   the supplier-risk signal is about) and the original-file button.
10. **A link in a page.** Open `order-4500131`: its History names the vendor as a link
    (Meier-Guss GmbH). Click it, or Tab to it and press Enter: the supplier opens in its own tab.
11. **Start a skill.** At the bottom of an order, the **Supplier risk for … with an agent** button.
    Its chevron (or the arrow-down key) offers: _Start in background_, _First make a plan_,
    _Start in terminal_, _View skill_. Start one in the background and watch the Runner view (right
    side): the run appears under _Running now_ with its elapsed time ticking, moves to _History_ as
    `ok · 6 s · now` when it ends, and the thread of that event grows a run.
12. **First make a plan.** Choose it: the runner drafts a plan and stops; a notification offers
    **Approve plan**. Nothing runs until you say so.
13. **Cancel and retry.** In the Runner view, open a live run and **Cancel run** (or use the stop icon on
    its row). A run that failed sits under _Needs attention_ with its reason on its own line: **Retry
    run** (the icon, or right-click) asks the runner again and tells you what happened. Requeue and
    Pause/Resume dispatch are there too, deactivated with the reason, because they are for admins.
14. **History and traces.** Under _History_ every past run is one line (`skill · target`, a short word, how
    long, how long ago), 25 at a time with **Load more…**. The funnel in the view's title filters it by
    status or skill. Click a run to open its detail: the plan, the attempts and the **trace**, a timeline
    of the tool calls with their outcome in words and a bar for how long each took; expand a call to see
    how much it sent and received (sizes, not content), and follow the link to what the run produced.

15. **Rows from outside systems.** Under _purchasing/suppliers_ two more skills are not escurel data at
    all: **supplier-rating** (a REST portal) and **delivery-confirmation** (an MCP server). `run.sh`
    starts both as real local processes (`services/ratings-api.mjs`, `services/confirmations-mcp.mjs`) and
    registers them as endpoints; the gateway reads them live. Open `iberica-forja` under supplier-rating: the
    strip says **External data (REST)** (hover: it is data, never instructions), the columns are the
    portal's and read-only, and the portal's URL is shown as the source.
16. **Change something at the source, with a reviewer.** In the strip press **Change rating…**, type `B`,
    add a note. Nothing has happened at the portal yet (`curl` the portal: still `A`). The proposal waits
    under _Awaiting you_; promote it. Now the portal says `B`, the page shows "Last change sent to the
    source …: applied.", and your note is the row's notes. Do the same on a delivery confirmation (status
    `open` → `confirmed`, over MCP).
    A third source is a real **SQL database**: under _sales/orders_, **orders-db** reads the rows of a SQLite
    file (`$HOME/.cache/escurel-demo/sqlite/orders.db`, made by `sources/orders-db/seed.mjs`; the gateway sees it
    only through a registered secret reference and `ESCUREL_SQL_FILE_DIRS`). Open `SO-100231`, press **Change
    status…**, type `shipped`, promote it from _Awaiting you_: one `UPDATE` runs on that row (check with
    `sqlite3 …/orders.db 'select order_no,status from orders'`), the others stay as they were.
17. **When it goes wrong, it says so.** Stop the ratings portal (`kill $(cat $HOME/.cache/escurel-demo/ratings.pid)`)
    and open a supplier-rating row again: the page still opens, flags the source as unreachable, and keeps
    your notes. A change promoted while it is down is retried a few times and then reported as failed; the
    draft stays open to promote again. A change proposed from a stale row is refused as a conflict.

## Evolve scenarios (optional)

Two scripted Anofox Evolve searches and the comparison pages that explain them. Everything is
synthetic and the proposals are scripted (no model, no spend); a comparison is a search-time replay on
the training instance. It explains what changed. It is not validation and says nothing about unseen data.

```
# evolve-agent must be built with --features synthetic-brain
ESCUREL_DEMO_EVOLVE_AGENT_BIN=/path/to/evolve-agent \
ANOFOX_EXTENSION_DIR=~/.anofox/profiles/duckdb-1.5.6-linux_amd64 \
  demo/run.sh start
```

The window opens on the assortment comparison page, and the Scenarios view is already wired to the demo's
Evolve service (no setting to change).

`run.sh` starts Evolve in OIDC mode against the demo gateway's issuer, runs the three searches as the demo
user (`evolve-scenarios.mjs`), and leaves one comparison page for each under the `evolve_comparison`
skill. In the window:

1. **Bin-packing** (`demo-bin-packing-vs-seed`): a scripted search finds a better packing. Open the page,
   click **Compute comparison**, then open the **Scenarios** view: `p0_bin_assignment` shows which items
   moved bins relative to the seed. Click the table to open the native diff.
2. **Assortment** (`demo-assortment-vs-top-n`): the classical "rank by margin × volume" shelf delists the
   whole Household category; the substitution-aware winner keeps four of its products and captures
   about 29% more margin on the training data. The comparison is against the winner's parent (the top-N
   program), so the diff shows exactly what the smarter assortment changed.
3. **Replenishment** (`demo-replenishment-vs-seed`): a scripted search moves a two-SKU policy from a
   batch every 6 days to a batch every 3 days, on a made-up sealed holdout. The comparison replays both
   policies on the training problem and diffs per SKU and day (`p1d_sku_day`: orders, arrivals, stock,
   cost) plus per-SKU service and totals. The sealed holdout is never read by a comparison, and nothing
   here validates a policy or says anything about real demand.

How it fits together: the page is the request and a readable summary (owner-private); Evolve keeps the
immutable result and the row-level changes. The page carries the result's hash, and the Scenarios view
shows a comparison only when that hash matches Evolve's record, so a page that merely claims to be
completed is shown as *unverified*. Create more with **New scenario comparison** in the view.

The same flow is tested in a real window by `test/e2e/evolve-scenarios.spec.ts` (set `EVOLVE_AGENT_BIN`).

## What CI runs, and what it cannot

- `vscode.yml`: typecheck, lint, unit, component, visual, the integration suite, and the Evolve plan-review e2e
  (`evolve.spec.ts`, `evolve-gemini.spec.ts`).
- `workbench-image.yml`: builds the web workbench image and runs `scripts/web-workbench-smoke.sh` and the login-page
  token test (path-filtered, and nightly).
- `image-build.yml`: builds the gateway image, the runner image and the `demo-gateway` target.
- NOT run in CI: `evolve-scenarios.spec.ts` and `evolve-service.spec.ts`. They need `EVOLVE_AGENT_BIN`, an
  `evolve-agent` built with `--features synthetic-brain` from the anofox-evolve repo, which this repo's CI does not
  build; without it they skip (they say so). Run them by hand: `EVOLVE_AGENT_BIN=... npm run test:e2e -- evolve-scenarios.spec.ts`.
  On a loaded machine set `ESCUREL_E2E_SLOW=4` to stretch every wait.

## Limits worth saying out loud

- The title bar reads "[Extension Development Host] Escurel": a window started with
  `--extensionDevelopmentPath` always carries that prefix. VS Code has no setting that hides the stock
  Explorer / Search / Source Control icons, so `demo/run.sh` pre-seeds the throwaway profile's state with
  them unpinned; the shipped focus mode does not do that.
- The runner is the echo harness: it folds the signal into the page, it does not reason. The
  lineage, the live updates and the review are real; the "agent" is a stand-in. The Runner view says so
  itself ("echo harness (demo, no AI model)"), so nobody mistakes the demo's runs for model output.
- The gateway runs with `ESCUREL_EGRESS_ALLOW_LOOPBACK=1` so that it may call the demo's local portal and
  MCP server; a real deployment refuses loopback and plain http (see `references/09` of the platform skill).
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

## Knobs (environment variables of the demo launcher)

None of these is an engine setting (they are not in `docs/deploy/env.md`); they only steer `demo/run.sh`,
the story driver and the S2D sync. Unset, every one has the default shown.

| Variable | Default | What it does |
| --- | --- | --- |
| `ESCUREL_DEMO_HOME` | `~/.cache/escurel-demo` | Where everything lives (gateway data, runner ledger, VS Code profile, bearer file). One demo per home; `stop` only stops the demo under this home. |
| `ESCUREL_DEMO_CODE` | `code` | The VS Code binary to launch. |
| `ESCUREL_DEMO_CODE_ARGS` | — | Extra VS Code arguments (`--ozone-platform=x11 --disable-gpu` on a machine with a wedged GPU). |
| `ESCUREL_DEMO_CDP_PORT` | — | Exposes the window's DevTools protocol on this port (screenshots, the e2e driver). |
| `ESCUREL_DEMO_FOCUS` | `1` | `0` keeps the classic IDE look instead of the calm focus view. |
| `ESCUREL_DEMO_ZOOM` | `1` | `window.zoomLevel` of the demo profile (`0` gives a wider Runs panel at 1600×1000). |
| `ESCUREL_DEMO_THEME` | — | A colour theme for the window (a themed tour of every screen). |
| `ESCUREL_DEMO_DIALOG_STYLE` | `native` | `window.dialogStyle` of the demo profile. |
| `ESCUREL_DEMO_OPEN_PAGE` | the Evolve comparison page when Evolve is seeded | A page id the bootstrap opens after sign-in. |
| `ESCUREL_DEMO_RUNNER_HARNESS` | `echo` | The runner harness; anything else is started as a second runner (needs the matching keys). |
| `ESCUREL_DEMO_EVOLVE_AGENT_BIN` | — | Path to an `evolve-agent`; set, it also turns on `ESCUREL_DEMO_EVOLVE_SEED`. |
| `ESCUREL_DEMO_EVOLVE_SEED` | `0` | `1` seeds the Evolve experiment / comparison pages. |
| `ESCUREL_DEMO_EVOLVE_ENDPOINT` | — | The Evolve endpoint the extension is configured with (left out unless the demo starts Evolve). |
| `ESCUREL_DEMO_SUBJECT` | `alice` | The owner subject the Evolve seed writes pages as. |
| `ESCUREL_DEMO_S2D` | `1` | `0` skips the Source-to-Deliver stories. |
| `ESCUREL_DEMO_S2D_REPO` (alias `S2D_HETZNER_REPO`) | `~/Projects/datazoo/hetzner-agent-substrate` | The checkout the S2D sync reads `ops/seed/s2d-demo` from (read-only). |
| `ESCUREL_DEMO_OPT_EXT` (alias `ESCUREL_DEMO_OPTIMIZE_EXT`) | the local builds `s2d/optional.py` knows | One `anofox_optimize.duckdb_extension` to load; used only when its footer matches the gateway's DuckDB. |
| `ESCUREL_DEMO_LIBDUCKDB_DIR` | the `target/` copy matching `Cargo.lock` | Directory of the `libduckdb.so` the gateway loads (see `docs/notes/discovered/2026-10-09-demo-run-sh-picked-a-stale-libduckdb.md`). |
| `ESCUREL_DEMO_AGENT_PAUSE_MS` | `1100` | Pause between the scripted agents' calls, so a seeded run's trace has a realistic spread. |
| `ESCUREL_DEMO_WAIT_MS` | `120000` | How long the story driver waits for a step (the first changeset, a run) before giving up. |
| `ESCUREL_CLI_BIN` (alias `S2D_CLI_BIN`) | `target/release/escurel` | The `escurel` CLI used for `migrate-kind-files` on the synced seed. |
| `ESCUREL_DEMO_RATINGS_URL`, `ESCUREL_DEMO_CONFIRMATIONS_URL`, `ESCUREL_DEMO_ORDERS_DB_SECRET` | set by `run.sh` | Where the demo's REST ratings API, MCP confirmations server and SQLite orders file are; `materialise.mjs` reads them. |
| `ESCUREL_DEMO_BEARER_FILE`, `ESCUREL_DEMO_STORY`, `ESCUREL_DEMO_DUCKDB_VERSION`, `S2D_GATEWAY_DUCKDB` | set by `run.sh` | Internal hand-offs to the bootstrap extension and the S2D sync; not meant to be set by hand. |

**Naming trap.** The Evolve agent the demo can start reads `ESCUREL_OIDC_ISSUER` / `_AUDIENCE` / `_JWKS_URI`
(and `EVOLVE_OIDC_*`). Those are the *agent's* verifier settings, not gateway keys: the gateway's are
`ESCUREL_AUTH_OIDC_*`. The agent-side names are being renamed `ESCUREL_EVOLVE_OIDC_*` in the agent (anofox-evolve),
with the old names kept as a deprecated alias for one release. The demo and the e2e fixtures already pass BOTH
spellings (`demo/evolve-env.mjs`), so they work with an agent build of either generation; once the agent only reads
the new names the old ones can be dropped there. What the agent repo must change: read `ESCUREL_EVOLVE_OIDC_*` first,
fall back to `ESCUREL_OIDC_*` with a one-time warning (`crates/evolve-agent/src/cli.rs`, and its tests that set them).
