# The recorded tool-call rows run 2 h ahead of the recorded run

**Symptom.** In `test/unit/fixtures/lineage/`, `run-tool-calls-page1.json` stamps the calls `2026-09-29T04:59:08Z`
while the same run's `started_at` is `02:58:18Z`. A trace that shows each call's offset from the run start
("+1 s") rendered `+120 min 50 s` for the recorded run, which took 175 ms.

**Cause.** The recording was made on a build/machine (CEST, UTC+2) that stamped tool calls in local time with a `Z`.
A live gateway does not: a run probed on 2026-10-04 stamped `run-started` and its first call `12:10:59Z` both.

**Fix.** None in the product. The visual harness (`test/visual/harness/run.ts`) starts the recorded run at its first
call. If offsets ever look hours off against a LIVE gateway, that is a real server clock bug: compare
`started.at` with `getRunToolCalls().calls[0].at` first.
