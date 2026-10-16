# MinIO no longer publishes images: the live tests moved to RustFS

**Symptom.** The nightly `live` workflow (`live storage + ducklake`) was red every morning
(2026-09-10, then again 2026-10-14, 15 and 16). Every S3 conformance test and every DuckLake live
test died in the container start, before touching any code under test:

```
start minio: Client(PullImage { descriptor: "quay.io/minio/minio:RELEASE.2025-02-28T09-55-16Z", ...
  status_code: 401, message: "unauthorized: access to the requested resource is not authorized" })
```

**Cause.** MinIO stopped publishing its community images. `minio/minio` on Docker Hub answered 404 first
(PR #505 moved the pull to quay.io); later `quay.io/minio/minio` lost every tag, `latest` included
(`docker manifest inspect` answers "no such manifest" for each). None of the four DuckLake live test
files ever had the quay.io override: they used `testcontainers_modules::minio::MinIO::default()`.

**Why it stayed hidden locally.** A developer machine that had pulled the image once keeps it:
the same tests pass there. Only a cold runner shows the failure. Remove the local MinIO tags
(`docker rmi`) to reproduce.

**Fix.** One helper, `escurel_storage::testing::S3Server` (feature `test-s3-server`), starts a pinned
RustFS container; the S3 conformance test and the four DuckLake live files use it, and
`live.yml` pre-pulls the same tag (a test, `s3_server_pin`, keeps the two in step).

## Choosing the replacement (evidence, 2026-10-16)

Each candidate: image pullable anonymously (`docker manifest inspect`), project active (latest release),
then the REAL `s3_roundtrip` suite (10 tests) run against a container of it, then the real DuckLake live
tests (httpfs, path-style, `USE_SSL false`) against the winner.

| server | image pullable | latest release | escurel S3 conformance (10) | notes |
|---|---|---|---|---|
| RustFS 1.0.1 | yes | 2026-10-03 | 10/10 | env-only config (`RUSTFS_ACCESS_KEY/SECRET_KEY`), signed requests checked; `/health` answers 200 before S3 calls are accepted (the helper retries bucket creation); 41/41 DuckLake live tests |
| SeaweedFS 4.48 | yes | 2026-09-28 | 10/10 | passes, but it was run with no identities configured, so the credentials were not checked |
| versitygw v1.9.0 | yes | 2026-10-13 | failed at bucket creation with a minimal run (not investigated) | needs backend config; not pursued |
| LocalStack 4.14 | yes | 2026-02-26 | failed at bucket creation with a minimal run (not investigated) | not pursued |
| Garage 2.1 | yes | (repo on its own forge) | not tried | needs a config file plus bucket/key provisioning through its CLI |

RustFS won: env-only, enforces the credentials, active, and it passes the real tests.

**Recognise it next time.** `PullImage ... 401/404` in a `start ...` expect in a live test, red nightly
with nothing in the diff; or a green local run that a cold CI run contradicts.

## Changelog lines for the release notes

- Tests (no consumer impact): the live S3 and DuckLake tests start RustFS (`rustfs/rustfs:1.0.1`)
  instead of the discontinued MinIO images; `escurel-storage` gains the opt-in feature
  `test-s3-server` (`escurel_storage::testing`).
