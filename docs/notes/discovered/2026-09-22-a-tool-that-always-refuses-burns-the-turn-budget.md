# A tool an agent is told to call, that always refuses, burns the whole turn budget

**Date:** 2026-09-22 · **Found while:** landing the run-lifecycle events
(workbench backend P1, PR7) against the live Gemini barrier test.

## Symptom

`workflow_end_to_end::verify_barrier_runs_against_gemini` failed
deterministically (twice) with *"only 2 of 3 verify-vote instances within
180s"*. The missing vote's harness line:

```text
dispatch: harness completed … harness "gemini" ok=false tool_calls=0
summary="stopped after 12 model turns without a final answer: "
```

Twelve turns, **zero counted tool calls**, an empty summary. The gateway
log for the same window showed one `report_progress` call answered
`invalid_params: requires a run-bound token`.

## Cause

Two things met. PR6b's packager paragraph tells every agent to call
`report_progress` *before you act*. That test's runner is a static-bearer
one (`ESCUREL_RUNNER_TOKEN`), so its per-run agent token carries no
`run_id` claim and the gateway refuses the call. The model did what it was
told, was refused, and kept trying variants until its turns were gone — the
Gemini adapter counts only *allowed* calls, so the loop was invisible in
`tool_calls`.

## Fix

Two halves, both in PR7:

- The packager appends the paragraph **only when the run's token can
  report** — a minted, run-bound bearer (`package()` knows: the
  `CallerToken::Agent` / `Scoped` arms with `run.is_some()`). A static
  dev runner packages without it. Don't tell an agent to call a tool that
  will refuse it.
- The Gemini adapter's denied-tool response now **names the allowed
  tools** instead of a bare "not allowed", so a model reaching for the
  wrong name can correct itself rather than retry it.

## How to recognise it next time

A harness outcome with `ok=false`, `tool_calls=0` (or a small constant)
and a "stopped after N model turns" summary is a model looping on
something it cannot do, not a slow model. Check what the instructions
*ask for* against what the token / tool list *allows* — and check the
gateway log for the refusal, because the adapter's counters only see
what it let through.
