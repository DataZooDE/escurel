# `admin_purge_events`: a supported way to trim an event label

**Date:** 2026-10-16. Companion of
[`2026-10-16-events-table-read-in-full-on-attached-postgres.md`](2026-10-16-events-table-read-in-full-on-attached-postgres.md).

## Why

Nothing in the engine deleted events except the keep-50 prune of `escurel:runner-status` (and run-progress). When a
deployment's heartbeats had piled up (a gateway older than #554) the only way to trim them was raw SQL against the catalog
database, which the operator rightly did not want.

## What

`admin_purge_events` (MCP, admin scope) / `escurel admin purge-events` / `AdminClient::purge_events`:

* names one label (`label_skill`, exact) and bounds the purge by `before_ts` (RFC 3339), `keep_latest` (the newest N
  survive), or both; with neither it is refused, so a purge never means "every row of the label" by omission
  (`keep_latest: 0` says it);
* **a dry run unless `apply: true`**: the report (`total`, `matched`, `kept`, `deleted`, `dry_run`) is the same either way;
* audit labels (`escurel:write-back`, `escurel:kind-migration`, `escurel:event-purge`) need `allow_protected: true`;
* an applied purge that deletes anything leaves a system event under `escurel:event-purge` carrying the report, so the
  purge itself is on the trail;
* scoped to the gateway's tenant on the shared backends, and on the attached-Postgres table the counts and the delete run
  inside Postgres (no rows cross the wire), on the lake and local tables as plain SQL.

## Not covered

It purges by label. Purging by instance, run or root is a different tool; chat has `admin_delete_chat_history`.

## Changelog lines for the release notes

* Added: `admin_purge_events` / `escurel admin purge-events` (admin scope): event-bus retention by label, age and/or newest-N,
  a dry run unless `apply`, audit labels protected, an applied purge leaves an `escurel:event-purge` event. The server
  exposes 87 tools.
