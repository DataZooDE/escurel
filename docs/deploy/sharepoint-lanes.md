# Lane store on SharePoint (`duckvfs`, `sharepoint://`)

Escurel's `duckvfs` backend hands paths to DuckDB's virtual filesystem, so a document library can hold the markdown lanes
the way a Shared Drive does for `gdrive://`. The `sharepoint` extension
([duckdb-sharepoint](https://github.com/DataZooDE/duckdb-sharepoint), published for DuckDB v1.5.6 on the erpl.io
mirror) provides the filesystem and the same `write_blob` / `remove_file` / `move_file` / `file_size` functions.

**Scope today: lanes only.** The DuckLake data path (`ESCUREL_DUCKLAKE_DATA_PATH`) and the image bake are separate steps;
nothing here changes the running lab. This is staged for a test on a non-production site.

## Configuration

```sh
ESCUREL_STORAGE_BACKEND=duckvfs
ESCUREL_STORAGE_DUCKVFS_ROOT=sharepoint://<library path>/lanes
ESCUREL_STORAGE_DUCKVFS_EXTENSION=/path/sharepoint.duckdb_extension      # or …_EXTENSION_REPO=http://get.erpl.io
ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_TENANT_ID=<Entra tenant>
ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_ID=<app registration>
ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_CLIENT_SECRET_FILE=/run/secrets/sharepoint-client-secret   # or …_CLIENT_SECRET
ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_DRIVE_ID=b!…        # exactly one of DRIVE_ID / SITE_ID
```

- Mount the secret as a file; the value is trimmed and never printed, logged or put in an error. `Debug` shows
  `<redacted>`.
- The extension is unsigned: `ESCUREL_ALLOW_UNSIGNED_EXTENSIONS=true`, as for gdrive.
- Config fails closed: every credential and exactly one target are required, and the `…_SHAREPOINT_*` variables are
  refused when the root is not `sharepoint://`.
- Authentication is a client secret (`client_credentials`). The extension has no workload-identity provider yet, so the
  pod needs the secret from Secret Manager; Entra secrets expire, plan the rotation. Use one app registration per
  environment: Graph throttling is per app per tenant, and a shared registration shares the budget.

## Limits that come from SharePoint

- File and folder names may not contain `" * : < > ? \ |`. A lane write whose key does is refused before any I/O and the
  error names the character. Check page ids that use `:`.
- Deleted files go to the site's recycle bin and count against quota.
- A move is not atomic (as on Drive); a write publishes only when the file closes, so a single write is.
- Path length is limited to about 400 characters.

## Testing

Offline: `cargo test -p escurel-storage --features duckvfs` and
`cargo test -p escurel-server --test suite duckvfs_sharepoint_config`. Three `#[ignore]`d unit tests in
`crates/escurel-storage/src/duckvfs.rs` register the secret with a real `sharepoint.duckdb_extension` (no network, no
tenant: the secret is registered, never spent); the comment above them names the variable that points at the extension
file and the command that runs them.

The live stage (lane round trip against a real library) needs a non-production site, a dedicated Entra app registration
limited to that site, and a credential file at mode 0600.
