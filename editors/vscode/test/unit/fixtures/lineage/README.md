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
| `lineage-cascade.json`, `events-cascade-root.json` | B1 | one cross-skill run: `list_lineage` with all node types, including the hop `cascade:<draft_id>` whose parent is the RUN, and `list_events { root_event_id, include_system: true }` for the same root. |
| `run-events.json`, `run-tool-calls-page1.json`, `-page2.json` | B2 | a **different run**: `list_events { run_id }` (carries review transitions as well as `escurel:run` rows, because they share the run id) and `get_run_tool_calls` with `limit: 2`. Its ids do not match B1's. |
| `run-detail-lineage.json`, `run-detail-events.json`, `run-detail-tool-calls-page1.json`, `-page2.json` | D | one run recorded WHOLE: the lineage, the run's events and both tool-call pages agree on the run id and the root. Use this set for anything that joins them. |
| `lineage-paged-full.json` | C | the unpaged `list_lineage` of recording C, taken after stopping the runner so the log could not move. |
| `lineage-paged-page1..9.json` | C | the same lineage with `limit: 1`, every page until `next_cursor` was absent. |

Recording C exists because B's two pages were not the whole read: page 2 still had a
cursor, and a model tested against it had to guess the run's final state. In C the run
first appears `processed` on page 6, and a changeset's parent is the root on early pages
because its run is not on them. Folding all nine pages in any order must reproduce the
full read exactly.

Every recording is a separate run, so ids differ between sets. Keep each set together, and never join a file from one set with a file from another: B1 and B2 were recorded separately, which is exactly how a test once joined two different runs without noticing.
