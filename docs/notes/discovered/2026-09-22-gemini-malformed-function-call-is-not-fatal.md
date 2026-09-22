# A Gemini `MALFORMED_FUNCTION_CALL` used to cost a whole run attempt

**Date:** 2026-09-22 · **Found while:** landing the packager's "report your
plan" paragraph (workbench backend P1, PR6b) against the live Gemini barrier
test.

## Symptom

`workflow_end_to_end::verify_barrier_runs_against_gemini` — green on every
earlier gate — failed with *"only 2 of 3 verify-vote instances within
180s"*. The runner log for the missing vote:

```text
dispatch: harness run failed … harness "gemini" upstream error:
generateContent returned no parts: {"candidates":[{"finishReason":
"MALFORMED_FUNCTION_CALL", "finishMessage":"Malformed function call:
print(default_api.report_progress(plan=[ default_api.ReportProgressPlan(
step='Report initial plan.', status='completed'), …]))"}], …
"modelVersion":"gemini-2.5-flash"}
```

`gemini-2.5-flash` answered the new instruction by *writing Python* for the
nested `plan` array instead of emitting a function call. The API reports
that as a candidate with a finish reason and **no `content`**; the adapter
read "no parts" as an upstream failure and the attempt died. The retry
policy then spent its budget on a model that does the same thing again.

## Cause

Nested array-of-object arguments (`plan: [{step, status}]`) are exactly
the shape this model sometimes renders as code. Any tool with such a
schema can trigger it — `report_progress` was merely the first one an
agent is *told* to call on every run.

## Fix

`GeminiHarness::converse` treats `finishReason == MALFORMED_FUNCTION_CALL`
as a turn to correct, not an error: it appends a text part to the **last
user turn** (keeping the user/model alternation the API expects) naming
the malformed call and asking for plain-JSON arguments, then continues
within the same `max_turns` budget. A model that never recovers runs out
of turns as a `FAILED` outcome the reconciler can retry — bounded, never
an adapter error. Pinned by
`crates/escurel-runner/tests/suite/gemini_malformed_call.rs` with a stub
model that scripts the live failure verbatim.

## How to recognise it next time

A Gemini run that "returned no parts" with `finishReason` set is the model
refusing or fumbling, not the API failing. Look at `finishMessage` before
treating it as infrastructure. Other finish reasons (`SAFETY`,
`RECITATION`, `MAX_TOKENS`) are still surfaced as upstream errors — they
are not the model's to fix on a retry.
