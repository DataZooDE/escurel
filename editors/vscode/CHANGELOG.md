# Changelog — escurel VS Code extension

The extension is released together with the engine: `release.yml` builds one VSIX per `v*` tag and
derives its version from the tag (`vYYYY.MM.DD` → `YYYY.M.D`, leading zeros dropped), so
`package.json` stays at `0.1.0` and is never bumped for a release. Entries name the escurel PR.

## Unreleased

### Added

- **Review and approve Evolve data rules**: `Discover, review and approve Anofox Evolve data rules…`
  proposes candidate rules for a prepared training source, runs the one confirmation test after you agree,
  shows the value-free report (counts, p-values, Holm-corrected decision, required rows, insufficient
  evidence), and approves only the supported rules you choose and then confirm. Approval is never automatic
  and is bound to the exact test you reviewed. The approved hash can be attached to a new replenishment
  experiment started from your own start file. Talks to Evolve directly (like holdout registration); the
  Evolve guided `evolve_run` path does not carry the hash yet.
- The Evolve event labels now come from one table (`escurel-types` and `shared/evolveControls.ts`, compared
  by a test); the gateway gates `evolve_rules_propose` / `evolve_rules_test` / `evolve_rules_approve` by
  label, owner and page state.

- **Focus mode** (#660): `Switch to focus view` / `Leave focus view` / toggle. The calm window hides the
  menu bar, command center, status bar, breadcrumbs, minimap and editor actions and moves the activity bar
  to the top; the person's own user-level settings are remembered on enter and restored exactly on exit.
  Asks once before changing settings.
- **Overview board** (#660): the first screen of the calm window, "Today": decisions waiting, agent
  activity, needs attention, open items per skill, recently finished; each line opens the thing it names.
- **Escurel Calm** light theme (#660) with WCAG contrast guards; thread connectors have arrowheads and at
  least 3:1 contrast in every theme, state chips are outlined green/amber/red with icon and word, buttons
  have borders on light surfaces.
- **Runs control center** (#651–#660): Running now / Waiting for you / Needs attention / History with a
  24 h summary, filters incl. Today / Yesterday / Last 7 days and "Runs for this record"; cancel asks first,
  retry says it starts a new run; control outcomes go to the status bar, toasts only for failures.
- **Run detail** (#660): a failure banner, human labels for tool calls with the raw name in the tooltip, a
  time axis with tick labels, and each step opens to what it asked and what came back
  (`args_summary` / `result_summary`, gateway `ESCUREL_TOOLCALL_DETAIL`).
- **Thread canvas** (#649–#651): lane rows per cascade branch, small finished cards, typed cards; a thread
  opens on the node that needs you; cards name their skill; journeys link page ↔ thread ↔ run ↔ skill.
- **Knowledge tree** (#654): folders, roles, backend icons, tag filter, previews of row sources; readable
  labels for external backends (SQL table / REST API / MCP tool).
- **Page-as-UI** (#654, #660): source-row banner, write-back status line, "Change <column>…" for writable
  columns of REST/MCP/SQL rows, report KPI figures and tables for records whose skill names a report,
  "Preview with parameters" on query pages.
- **Skill page** (#654): a readable page for a skill with its actions, fields, facts and records.
- **Explain this view** on every view with a glossary; "Plan ready" rows in Awaiting you; Open buttons on
  notices; editor tabs name the page or skill, not the URI; keybindings `ctrl+alt+r/a/i/k`.
- **Scenarios view** (#658): Evolve seed-vs-winner diffs and comparison pages, "New scenario comparison",
  the Compute comparison action.
- **Welcome states** for quarantined, old-server, 401 and unreachable gateways; a first-run walkthrough.
- **Delayed-items tile on the Overview board** (#700): a skill that declares `overview:` in its frontmatter gets
  a tile ("N delayed", the top three by the declared sort field, each line opens the instance). Reads are
  bounded (5 pages x 100): a spent budget reads "N+ delayed"; a rows skill whose status field is not
  `filterable` gets no tile.
- **Harness picker on Start** (#701): when `escurel.harness` is unset and the runner publishes more than one
  allowed harness (`harness_allow`), Start asks which one ("Use default" first). A configured name that the
  runner does not allow shows a warning (Start with default / Open settings / Cancel); nothing outside the
  published list is ever sent. An older runner behaves as before. The Runs tooltip lists the allowed harnesses.
- **Close and reopen a thread** (#702): `Close thread` (asks first, reason optional) and `Reopen thread` in the
  palette and the outline menu; the Threads outline ends in a collapsed **Closed** group (title, who closed it,
  when, reason). Closing is shared and visible: anyone who can read the thread can reopen it. Nothing in the
  thread is deleted; Overview, Awaiting you and Inbox are unaffected.
- **Explain this view** also on the Scenarios view and inside the Details panel (#690).
- **First-connect suggestion** (#698): the first time the extension connects, and not in focus mode, it offers
  once to switch to the calm focus view (never in the demo and web workbench windows, which enter it
  themselves, nor in untrusted or virtual workspaces).

### Changed

- The typed client treats `isError: true` as an error, never as an empty result (#654); `validate` refusals
  surface as a diagnostic.
- Untrusted text (run reasons, skill facts, OKF strings, labels) is cleaned and capped at the model
  boundary (#654).
- Approving a plan opens the run first; Approve is an explicit, confirmed action (#654).
- The quiet Overview board says what is actually true per tile ("Nothing has finished yet", "No records yet")
  instead of "All clear" everywhere; the Details panel aligns its key/value pairs on one baseline and omits a
  label whose value is empty (#698).
- In the zoomed-out thread overview the column headers no longer run together: below 85% zoom a header shows
  only its leading stage name (full label stays in the accessible name); cards keep icon and text (#699).

### Fixed

- The Closed threads group loads when the window starts and after every reconnect, and reads the newest marks
  first, so a reopen or a recent close in a long history is no longer missed (#707, #711).
- Approve plan no longer sends an `escurel.harness` the runner's allow-list does not name; the runner default
  is used instead. The delayed-items count is capped (5 pages, shown as "N+ delayed") (#710).

### Demo

- `demo/run.sh` opens the calm window by default (`ESCUREL_DEMO_FOCUS=0` for the classic look), picks the
  libduckdb matching `Cargo.lock`, and seeds the Source-to-Deliver stories (`demo/s2d/`, see
  `demo/s2d/REHEARSAL.md`).
- The s2d story waits for the gateway (bounded, with a clear message) instead of failing when started early; the
  demo shell tests run in CI (#709).
