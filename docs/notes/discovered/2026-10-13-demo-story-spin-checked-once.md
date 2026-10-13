# demo story.sh `spin` checked once and the shell tests never ran in CI

**Symptom.** Found by the second crew review. In s2d mode `story.sh` is meant to wait for the gateway state
(gateway.json, bearer.json, two /healthz) but `spin` just `eval`ed its condition once, so a story started a
little before the gateway failed outright (or ran with an empty port). `test-init.sh`/`test-story.sh` stubbed
the condition to always pass, so nothing noticed; neither ran in CI, and `test-story.sh` was not executable.

**Fix.** `spin` loops once a second until the condition holds, gives up after `DEMO_STORY_WAIT_SECS` (default
300) with `story: gave up after Ns waiting for: <condition>` and exit 1. `test-story.sh` has a leg where the state
files appear after 2 s and one where they never do. Both shell tests now run first in the `fmt + clippy + test`
CI job (seconds, sh + node only).

**Recognise it next time.** A test stub that always succeeds hides a wait that never waits: give every wait a
test where the condition becomes true late and one where it never does.

## Changelog lines for the release notes
- Demo: the s2d story waits for the gateway (bounded, with a clear message) instead of failing when started early.
