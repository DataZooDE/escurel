# events-copy opened a bare DuckDB connection and refused the unsigned gdrive extension (2026-10-18)

**Symptom.** In the Drive rehearsal #982 `escurel-server events-copy` (dry run and apply) died while attaching the
`gdrive://` lake with `IO Error: ... signature is either missing or invalid` at `LOAD` of the gdrive extension, even
with `ESCUREL_ALLOW_UNSIGNED_EXTENSIONS=1` in the environment. The lab image bakes exactly that unsigned mirror
artifact, so the lab Job would have failed the same way.

**Cause.** `events_copy` used `Connection::open_in_memory()`. Whether unsigned extensions load is decided when the
database is OPENED (`Config::allow_unsigned_extensions`, it cannot be `SET` later); every other lake connection opens
with `Migrator::connection_config()`, which reads the env var. The copy was the odd one out.

**Fix.** `open_copy_connection()` opens with `Migrator::connection_config()`. Test: a child process with the variable
set must see `current_setting('allow_unsigned_extensions')` true on the connection the copy opens (the workspace forbids
`unsafe`, so the test cannot `set_var` and spawns itself instead).

**Recognise it next time.** A new command that attaches the lake and runs on its own connection: grep for a bare
`Connection::open_in_memory()` / `open()` in `snapshot/` and the admin commands. The only other bare one found is the
reader's CRDT connection in `config.rs` (~1962), which attaches Postgres only (a signed extension) and is fine.
