# A machine could switch the autonomy gate off, approve itself, or walk the egress check

Found by the second crew review (2026-10-04) and reproduced against real gateways before each fix.

**Symptom.** `autonomy: review` held a machine's write, and then a machine could (1) `update_page` the
SKILL page to `autonomy: auto`, (2) `promote_draft` the draft it had just been held in, (3) use a run token
minted by the runner, which is admin and was waved through the gate, (4) land bytes through `move_page`
(destination), `merge_branch`, `/ingest` (built its ACL caller with no run claims) or `write_instance`.
Separately the SQL-source egress check judged a Postgres DSN by splitting on whitespace and `=`, so
`host=a host = 127.0.0.1` and `?hostaddr=127.0.0.1` walked a private address past it, and `json_dir`
globs were not under `ESCUREL_SQL_FILE_DIRS` at all.

**Fix.** The gate asks `is_machine_caller` (run / skill / act claims) and no longer exempts admin; skill
pages are always held for a machine; promotion is a human act. DSNs are parsed with libpq's grammar
(`escurel_index::dsn`), keys are allow-listed, hosts are pinned; directory globs are confined.

**Recognise it next time.** A security rule written as "X is exempt" (`is_admin ||`) is only right for the
caller you pictured: ask who ELSE carries that role. A check that re-implements a driver's parser will
disagree with it; parse with the driver's own rules (or a faithful port) and refuse what you cannot parse.
