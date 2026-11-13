# A claim lease held by `<host>:<pid>` outlives the process that held it

**Symptom (caught in design, not in production).** With claim leases (#818)
keyed on the runner's id, a runner killed mid-run and restarted on the same
ledger would not resume its own pending event for a full lease (60 s by
default): the gateway still sees the old holder's live lease and answers
`claimed: false`. Every runner suite test that restarts a runner
(`RunningRunner::restart`) and expects a quick re-drive would stall.

**Cause.** `ESCUREL_RUNNER_ID` defaults to `<HOSTNAME>:<pid>`, so a restart is
a new id, and a new id is a new holder. The default is fine for status
records and `runtime` pages (a restart reads as a new runtime, the old one goes
stale), but it is the wrong identity for a lease: what makes a restarted runner
"the same runner" is its **ledger**, which carries the runs it owns.

**Fix.** The lease holder is `<token sub>#<ledger identity>`: `Ledger::identity`
mints a ULID into a `ledger_meta` row on first use and returns it ever after.
A restart on the same ledger keeps its leases; a fresh ledger waits out the old
lease, which is the behaviour a different runner should get. The holder is
written on the `runtime` page as `claim_holder`, so a reader can map a lease to
a runtime.

**Recognise it next time.** A runner-side identity that must survive a restart
belongs in the ledger (DuckDB), not in an env default or the pid. If a
multi-runner test passes but a restart test now waits exactly one lease, look
here first.
