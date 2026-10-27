# Chat latency: cold claude vs gemini (2026-10-27)

**What.** Time from a `chat` capture to the run's first `report_progress`, and to the
assistant's answer being appended, for the governed chat agent (BRD NFR-P2: measure,
target ≤ 20 s p50 for a 3-tool answer, no hard gate in v1).

**How.** `scripts/chat-latency.sh <harness> 3`: `escurel-test-gateway` (verifying) seeded
with the CRM demo corpus plus the chat corpus, `escurel-runner` (release) in minted mode,
poll intake at 250 ms (no webhook). One question that needs several reads — *"Which
customers do we have, and which of them has an open opportunity? Cite the pages you
used."* — asked three times; every run spawns a fresh harness process, so every run is
cold. One workstation (32 cores), one run at a time.

| harness | model | first progress (s) | answer (s) | tool calls / turns |
|---|---|---|---|---|
| gemini | `gemini-2.5-flash` (runner default) | 5.5 · 2.4 · 1.9 — **p50 2.4** | 18.5 · 19.2 · 6.3 — **p50 18.5** | 8 · 9 · 4 |
| claude | Claude Code 2.1.310, CLI default | 14.0 · 10.2 · 10.9 — **p50 10.9** | 33.6 · 19.7 · 23.8 — **p50 23.8** | 7 · 6 · 6 |

Every answer was grounded: each listed the three customers with `[[customer::…]]`
citations, and none was refused or failed.

**Reading it.**

- gemini meets the 20 s p50 target; claude misses it by about 4 s.
- claude reports its first progress ~8 s later than gemini, and the gap between first
  progress and answer is similar for both (gemini ≈ 4–16 s, claude ≈ 10–20 s). So the
  difference sits before the model's first tool call, which points (not measured
  separately) at the CLI starting, connecting to `/mcp` and listing tools — a cost paid
  per run, because the adapter starts `claude -p` for each one. Warm sessions, the
  mitigation the HLD defers, would remove it.
- Variance is large (gemini 6–19 s) and comes from how many tool turns the model
  chooses (4 to 9 here), not from the stack around it.
- Intake here is the 250 ms poll. With the 30 s default poll and no webhook, add up to
  30 s to every number: **configure `ESCUREL_WEBHOOK_URL` to the runner for chat**
  (NFR-P1; references/11 says so).

**Recognise it next time.** If "time to answer" regresses, run the script against both
harnesses before suspecting the gateway. If first progress grows but the answer does
not, look at harness start-up (CLI version, MCP handshake), not at the model.
