# The Drive lane store had no retry: one `temporary Drive API failure` ended a 20 minute boot

**Symptom.** heron-escurel (Drive lane) died at minute 20 of a cold boot: `escurel-server failed to start: building indexer:
lane store error: io error: duckvfs write: IO Error: temporary Drive API failure while accessing gdrive://…`. The pod
restarted and began the 20 minutes over. Google Drive (and Graph, for `sharepoint://`) answers an occasional transient
error (`temporary Drive API failure`, 429 / `rateLimitExceeded`, 5xx, a timeout); a boot that touches hundreds of lane
objects meets one sooner or later.

**Cause.** `DuckVfsStore` surfaced every DuckDB error as `StoreError::Io` and nothing repeated it. (The write that hit it
was itself needless: a rebuild re-wrote every page, see `2026-10-21-rebuild-rewrote-every-lane-page.md`. Reads and the
lists a boot makes are exposed the same way.)

**Fix.** `escurel_storage::RetryingStore` wraps any `LaneStore`: a transient `Io` error is repeated with exponential
backoff (500 ms, doubling, capped at `ESCUREL_STORAGE_DUCKVFS_RETRY_MAX_BACKOFF_MS`, jittered into its upper half) for at
most `…_RETRY_ATTEMPTS` tries and `…_RETRY_TOTAL_MS` per operation (defaults 6 / 30 s / 180 s). `NotFound`, permission,
quota (`storageQuotaExceeded`) and bad-key errors are permanent and returned at once. A retried `delete` that finds the
object already gone counts as done (the first answer was lost). The server wraps the duckvfs store with it; each retry logs
one `warn` line with the operation, the tenant-relative path, the attempt and the matched marker (never the error text).
Classification is by message text because the DuckDB filesystem surfaces Drive failures only as text: a new transient
wording needs a line in `TRANSIENT_MARKERS` (`crates/escurel-storage/src/retry.rs`).

**Not covered.** DuckLake over `gdrive://` (the lake's Parquet and the events/chat/drafts lake tables) talks to Drive inside
DuckDB, not through the lane store; a transient error there fails that statement. The snapshot publish already retries on its
next cycle, but a lake ATTACH at boot has no retry yet.

**Recognise it next time.** `warn` lines `transient lane store error, retrying` mean the retry saved an operation; a boot
that still dies with `temporary Drive API failure` after the budget means Drive was down longer than
`…_RETRY_TOTAL_MS`.
