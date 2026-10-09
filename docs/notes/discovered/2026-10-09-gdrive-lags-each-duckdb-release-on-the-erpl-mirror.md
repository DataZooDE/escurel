# The image's `gdrive` extension comes from a mirror that lags each DuckDB release

**Symptom.** Bumping the image to DuckDB 1.5.6 made the `extensions` stage fail: the Dockerfile
installs `gdrive` from `https://get.erpl.io/<duckdb version>/linux_amd64/`, and the mirror had a 1.5.5
artifact but answered 404 for 1.5.6. The DuckDB community repository DID have a 1.5.6 `gdrive`, but an
OLDER one (v2026.08.07) whose `credential_chain` refuses `external_account`: swapping it in would have
built fine and broken workload identity federation silently at runtime.

**Fix.** `Dockerfile`: the `gdrive` bake is conditional. When the mirror has no artifact for
`DUCKDB_VERSION` the image is built WITHOUT gdrive and says so loudly in the build log
(`REQUIRE_GDRIVE=1` fails the build instead); the baked-extension assertion only expects `gdrive` when
the mirror had it. The consequence: `ESCUREL_STORAGE_BACKEND=duckvfs` (the Google Drive lane store) is
unavailable in such an image. The 1.5.6 `gdrive` was then built from `DataZooDE/duckdb-gdrive`
(`build/duckdb-1.5.6`, merged as its PR #15) and its main-branch pipeline published it to the mirror.

**How to recognise it.** A `#####` WARNING block in the image build log; `docker run … duckdb -c "LOAD
gdrive"` failing inside the image; `curl -sI https://get.erpl.io/v<X>/linux_amd64/gdrive.duckdb_extension.gz`
returning 404 for the pinned version.

**Rule.** Every DuckDB bump needs the mirror checked for `gdrive` (and the other DataZoo extensions)
FIRST; the extensions are version-locked and the mirror is published by each extension repo's own
pipeline on a push to its main.
