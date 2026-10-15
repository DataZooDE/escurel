# `lake-upgrade` died on a fresh volume: "Can't find the home directory"

**Symptom** (the lab cutover of 2026-10-15, first step of `migrate-kind-job`, `helm --atomic` rolled back):

```
escurel-server: fatal: lake SQL failed: IO Error: Can't find the home directory at '/tmp/escurel'
Specify a home directory using the SET home_directory='/path/to/dir' option.
migrate-kind-job: FAILED: lake-upgrade failed (see above)
```

**Cause.** DuckDB resolves its home directory from `$HOME` and refuses to open at all when it is missing. The
substrate pods set `HOME` (and `ESCUREL_SERVER_DATA_DIR`) to `/tmp/escurel` on a fresh emptyDir. Only a *booting*
server created that directory (`EscurelConfig::serve`), and `escurel-server lake-upgrade` runs before any server has
booted. The rehearsal containers had the directory from earlier boots, so rehearsals 1 and 2 never saw it.

**Fix.** `EscurelConfig::ensure_data_dir()` creates the data dir (mode 0700) and, when `HOME` is set, the DuckDB home;
`lake-upgrade` and the server boot both call it, and a directory that cannot be created fails with a message naming
`ESCUREL_SERVER_DATA_DIR / HOME`. `scripts/migrate-kind-job.sh` also `mkdir -p`s both before its first command.

**Recognise it next time.** Any DuckDB-touching command run as the very first thing on a fresh volume fails with
`Can't find the home directory at '<HOME>'`. Tests that model a fresh volume must point `ESCUREL_SERVER_DATA_DIR` and
`HOME` at a path that does not exist yet (`lake_upgrade_cmd`, `migrate_kind_job_script`).

## Changelog lines for the release notes

- `escurel-server lake-upgrade` (and so `migrate-kind-job`) creates its data directory and the DuckDB home itself, so
  it works as the first step on a fresh volume or Job emptyDir; an uncreatable directory fails with a message naming
  `ESCUREL_SERVER_DATA_DIR`. The server boot shares the same helper.
