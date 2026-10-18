# The silent 21 minutes of a cold boot were the index open, not the lake adopt (2026-10-18)

**Symptom.** Drive rehearsal #982 (events and chat on the lake, fresh data directory): no log line for 21 min, then
`writer boot: adopted existing lake content` and `listening` five seconds apart. #746 had wrapped `attach_lake` and
`adopt_lake_for_writer` in `with_progress`, yet not one `boot: ... is still running` line appeared. heron-escurel's
live cold boot (19 min) looked the same.

**Cause.** Not log filtering (`RUST_LOG=info`, every target passes), not a starved ticker. The long step ran BEFORE
the two wrapped ones: `SingleFileStore::open` on a fresh data directory scans every lane page for legacy kinds
(`quarantine_legacy_kind_pages`) and rebuilds the whole index from the lane store (`rebuild_skipping_unparsable`):
one read per page over the Drive lane store, plus embedding. The rollback boot (an existing local index skips both)
took 5 min against 21. The adopt that followed took seconds because the lake content was already local.

**Fix.** `boot_progress::open_index` wraps the open (`config.rs`), phase text "open the index (a fresh data
directory scans and rebuilds it from the lanes)". Test: `boot_progress_open.rs`, a real `FsStore` behind a wrapper that
pays a delay per read and list, real DuckDB and rebuild, a 100 ms interval, at least three ticks.

**Recognise it next time.** A boot that is silent but not in a step with a `with_progress` around it. The order of a
writer boot is: open the index (fresh: scan + rebuild) -> attach the lake -> adopt -> attach events/chat/drafts.
The `attach_*_lake` calls for events, chat and drafts are not wrapped; they took seconds in the rehearsal, but wrap
them if a lake with many files ever makes them the long pole.
