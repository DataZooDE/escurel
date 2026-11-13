# DuckDB: replaying an `ALTER TABLE … ADD COLUMN` from the WAL crashes when the table has a `current_timestamp` default

**Symptom.** A process opens its DuckDB file, runs a migration
`ALTER TABLE turns ADD COLUMN IF NOT EXISTS admitted_ms BIGINT`, and is
killed before DuckDB checkpoints. The next open fails:

```
INTERNAL Error: Failure while replaying WAL file "…/bridge.duckdb.wal":
Calling DatabaseManager::GetDefaultDatabase with no default database set
```

The stack shows `WriteAheadLogDeserializer::ReplayAlter` →
`DuckTableEntry::AddColumn` → `Binder::BindDefaultValues`: replaying the
`ADD COLUMN` re-binds every column's default, and `DEFAULT current_timestamp`
needs a default database that does not exist yet during replay. The file is
unopenable until the WAL is removed (losing its writes).

Seen with libduckdb 1.5 (`duckdb` crate 1.10506) in `escurel-chat-bridge`'s
state store, whose integration tests SIGKILL the bridge to test restarts.

**Fix.** Do not ALTER at open. The column went into the `CREATE TABLE`
(the store had never shipped). A schema that must migrate a table with a
function default should `CHECKPOINT` right after the ALTER, so the change is in
the database file rather than only in the WAL, or avoid function defaults and
write the value explicitly.

**Recognise it.** A DuckDB file that worked, a kill/crash, then
`Failure while replaying WAL` + `GetDefaultDatabase` on the next open.
