# A process that announces readiness must already be able to be told to stop

**Symptom.** `escurel-test-support`'s `sigterm_shuts_it_down_cleanly` failed once in CI (not
locally, not on re-run) with `a SIGTERM must be a clean exit, got ExitStatus(unix_wait_status(15))`:
the process died from SIGTERM's default action instead of exiting 0.

**Cause.** `escurel-test-gateway` printed its connection line and only THEN installed its SIGTERM
handler. A parent that reads the line and terminates the child at once (the test, and any harness)
can land in the gap on a slow, contended runner. Status 15 is the tell: "killed by the signal", not
"exited".

**Fix.** Install the handler as the first statement of `main`, before anything slow, so a signal
that arrives during startup is remembered and ends the wait at once.

**Honest limits.** It did not reproduce locally: 0 of 40 runs under full-CPU load, old code and new.
The fix rests on the CI evidence (exit status 15) and on the ordering now having no window after
the line. A test that signals right after `spawn` is NOT a regression test: it lands before `main`
has even started, a window no user code can close (15 of 15 failures with the fix in place).

**Recognise it next time.** Any process that prints "ready" on stdout for a supervisor to read must
have its signal handlers in place BEFORE that line. An exit status of 15/143 from a child you
terminated yourself means it had none.
