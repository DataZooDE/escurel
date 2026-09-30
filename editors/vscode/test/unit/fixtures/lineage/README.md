# Recorded lineage fixtures

Real gateway payloads, captured from a **real `escurel-server`, a real `escurel-runner`
and the real echo harness**, with the runner in minted mode so per-run tokens stamp the
run onto its drafts. Nothing here is hand-written. Do not edit these files: re-record.

They live in a subdirectory on purpose. `mockGateway.ts` replays only the top-level
`fixtures/*.json` as `/mcp` answers; these are read directly by the model tests.

## How they were recorded

A temporary test in `crates/escurel-runner/tests/suite/`, built from the setup of
`lineage_end_to_end.rs`, printed each payload and was then deleted. The cascade needs
three conditions at once (see `runner-core/src/cascade.rs`), which is why the corpus is
shaped as it is:

- skill `signal`: `autonomy: review`, `actions: [order]`;
- skill `order`: `autonomy: review`, `cascade: { target: produced }`;
- one `order` instance; the trigger is labelled `signal` but names that `order` instance,
  so the run's write is cross-skill.

The trigger was captured, the run held a changeset, a human promoted it, and the runner
emitted the cascade hop.

## Files

| file | recording | what it is |
|---|---|---|
| `lineage-event-run-changeset-draft.json` | A | `list_lineage` for a single-skill review run a human promoted: event → run → changeset → draft. |
| `lineage-cascade.json` | B | `list_lineage`, all node types, for the cross-skill run above, including the hop `cascade:<draft_id>` whose parent is the RUN. |
| `run-events.json` | B | `list_events { run_id }` for that run. Carries review transitions as well as `escurel:run` rows, because they share the run id. |
| `events-cascade-root.json` | B | `list_events { root_event_id, include_system: true }`. |
| `run-tool-calls-page1.json`, `-page2.json` | B | `get_run_tool_calls` with `limit: 2`. |
| `lineage-paged-full.json` | C | the unpaged `list_lineage` of recording C, taken after stopping the runner so the log could not move. |
| `lineage-paged-page1..9.json` | C | the same lineage with `limit: 1`, every page until `next_cursor` was absent. |

Recording C exists because B's two pages were not the whole read: page 2 still had a
cursor, and a model tested against it had to guess the run's final state. In C the run
first appears `processed` on page 6, and a changeset's parent is the root on early pages
because its run is not on them. Folding all nine pages in any order must reproduce the
full read exactly.

Recordings B and C are separate runs, so their ids differ. Keep each set together.
