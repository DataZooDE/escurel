# Changelog — escurel VS Code extension

The extension is released together with the engine: `release.yml` builds one VSIX per `v*` tag and
derives its version from the tag (`vYYYY.MM.DD` → `YYYY.M.D`, leading zeros dropped), so
`package.json` stays at `0.1.0` and is never bumped for a release. Entries name the escurel PR.

## Unreleased

### Added

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

### Changed

- The typed client treats `isError: true` as an error, never as an empty result (#654); `validate` refusals
  surface as a diagnostic.
- Untrusted text (run reasons, skill facts, OKF strings, labels) is cleaned and capped at the model
  boundary (#654).
- Approving a plan opens the run first; Approve is an explicit, confirmed action (#654).

### Demo

- `demo/run.sh` opens the calm window by default (`ESCUREL_DEMO_FOCUS=0` for the classic look), picks the
  libduckdb matching `Cargo.lock`, and seeds the Source-to-Deliver stories (`demo/s2d/`, see
  `demo/s2d/REHEARSAL.md`).
