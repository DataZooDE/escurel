# A cold-boot rebuild wrote every page back to the lane (and one Drive error killed the boot)

**Symptom.** heron-escurel (lake on Google Drive, 223 pages) booted from a fresh data directory for ~20 minutes and then
died: `escurel-server failed to start: building indexer: lane store error: io error: duckvfs write: IO Error: temporary
Drive API failure while accessing gdrive://escurel/lanes/tenants/…/markdown/instances/email/…`. The pod restarted and
started the 20 minutes over. A boot that only READS the lane was WRITING to it.

**Cause.** `Indexer::rebuild` re-derived the index through `update_page_as`, and `update_page_as` writes the lane first
(it is the write path: lane = canonical, index = derived). A rebuild reads each page from the lane, then wrote the same
bytes back: one lane write per page. On a filesystem lane that is invisible; on a Drive lane it is one Drive API write
(~1.5 s) per page, a large share of the cold boot, and each one is a chance for a transient error to fail the whole
`Indexer::new` -> boot.

**Fix.** `update_page_inner(…, write_lane)`; `update_page_as` keeps `true`, `rebuild_inner` passes `false`. The meta-skill
guard, parse, embed, the index transaction, the attribution map and the mutation epoch are unchanged. `migrate-kind`
still writes the lane (it changes the bytes). `tests/suite/rebuild_no_lane_writes.rs` counts the writes a real `FsStore`
receives (0 now, 3 before) and compares the index a cold rebuild derives with the one the write path built.

**Recognise it next time.** A boot or `admin rebuild` that logs lane (`duckvfs write`, `PutObject`) activity or is slow on a
remote lane; a read-only lane store (permission denied on write) that cannot boot. A test that injects store faults must
not assume the rebuild writes: `migrate_kind_crash` used the rebuild's writes as its fault point and now fails the marker
delete instead.

Related: the lane store itself did not retry a transient Drive error (a separate fix: bounded backoff in the duckvfs store).
