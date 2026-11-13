# claude stream-json: `usage` is per turn, `total_cost_usd` is the session's

**Symptom.** A multi-turn `claude -p --input-format stream-json
--output-format stream-json` session (one `result` envelope per user turn —
what an injected message produces, #820) reported only the last turn's
tokens on `run-finished.usage`: the adapter read the LAST `result` only.

**What is true** (measured 2026-11-13, claude-code 2.x, two turns on haiku):

| field on each `result` | semantics |
|---|---|
| `usage.{input,output,cache_*}_tokens` | that turn only (turn 2's `output_tokens` 49 = its one message, not 80 + 49) |
| `total_cost_usd` | running total for the whole session (0.2039 → 0.2136) |
| `modelUsage.<model>.*` | running totals too (`outputTokens` 80 → 129) |
| `num_turns` | model turns in that user turn — not a tool-call count |

**Fix.** `session_usage` in `crates/escurel-runner-harness/src/claude.rs`
sums `usage` over every `result` and takes cost and model from the last one.
`tool_calls` counts `tool_use` items in the stream (the transcript parser),
not `num_turns`.

**Recognise it next time.** Any per-turn envelope stream: check with a real
two-turn session whether each counter is per-turn or cumulative before
summing or taking the last — the two kinds sit side by side in one object.
The recorded stream is pinned in the test
`a_multi_turn_session_sums_per_turn_tokens_and_takes_the_running_cost`.
