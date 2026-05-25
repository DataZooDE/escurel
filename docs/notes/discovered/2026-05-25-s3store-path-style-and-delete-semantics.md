# S3Store: path-style addressing + S3 delete is idempotent (no 404)

**Date:** 2026-05-25
**Area:** `crates/escurel-storage/src/s3.rs`

## Symptoms

Two traps surfaced while bringing up the `S3Store` LaneStore backend
against a real MinIO testcontainer.

### 1. Virtual-host addressing 403s / DNS-fails on MinIO + Hetzner

The AWS SDK defaults to **virtual-host-style** addressing
(`https://<bucket>.<endpoint>/<key>`). MinIO and Hetzner Object
Storage do not provision per-bucket DNS, so the bucket-as-subdomain
request either fails to resolve or signs against the wrong host and
returns 403/SignatureDoesNotMatch. The symptom is confusing because
`create_bucket` may appear to work while `get_object` fails.

### 2. `delete` of a missing key returns success, not NotFound

S3 `DeleteObject` is **idempotent**: deleting a non-existent key
returns `204 No Content`, not `404`. A naive `delete -> map 404 to
NotFound` therefore never produces `NotFound`, breaking parity with
`FsStore` (whose `delete_missing_returns_not_found` test expects the
error). Likewise `head_object` on a missing key returns a bodyless
`404` with **no** typed `NoSuchKey` service-error variant — unlike
`get_object`, which *does* carry `GetObjectError::NoSuchKey`.

## Fix

1. **Force path-style** on the client config:
   `aws_sdk_s3::config::Builder::new().force_path_style(true)`.
   This makes the SDK emit `https://<endpoint>/<bucket>/<key>`,
   which MinIO, Hetzner, and AWS all accept.

2. **HEAD-then-DELETE** in `delete()` to honour the trait's
   `NotFound` contract: `head_object` first; if it 404s, return
   `StoreError::NotFound`; otherwise `delete_object`. For the HEAD
   404 there is no typed variant, so match on the raw HTTP status
   via `SdkError::raw_response().status().as_u16() == 404`. For
   `get_object`, prefer the typed `is_no_such_key()` check.

## How to recognise it next time

- A new S3-compatible endpoint that 403s on reads but the bucket
  "exists": you forgot `force_path_style(true)`.
- A delete/HEAD path that never yields `NotFound`: remember S3 GET
  carries `NoSuchKey`, but HEAD and DELETE do not — fall back to the
  raw 404 status (HEAD) or HEAD-before-DELETE (DELETE).

## Versions

- `aws-sdk-s3` / `aws-config` / `aws-credential-types` = `1.x`
  (`aws-sdk-s3 1.133.0` at time of writing).
- MinIO image `minio/minio:RELEASE.2025-02-28T09-55-16Z` via
  `testcontainers-modules` `0.15` (`minio` feature) on
  `testcontainers` `0.27`.
- `S3Store` is gated behind the `s3` Cargo feature on
  `escurel-storage`; the integration test lives at
  `crates/escurel-storage/tests/s3_roundtrip.rs`.
