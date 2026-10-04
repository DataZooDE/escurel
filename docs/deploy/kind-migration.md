# Runbook: upgrading across the `type:` → `kind:` release (escurel skill 0.8.0)

**BREAKING.** The page-kind frontmatter key `type: skill|instance` was **removed**; it is `kind:` now (also
`page_type` → `page_kind` on the wire, and the workflow-run page's `status` → `run_status`). A tenant that
still holds pages with the old key boots **QUARANTINED**: the server is up, but every MCP tool except
`migrate_kind` and `compact_lanes` answers `tenant_quarantined`, and `rebuild` refuses. The migration runs
against that tenant, from inside the same image.

Consumers (repos of page files, services that write pages): [`consumer-rollout.md`](consumer-rollout.md). A repository's own files migrate offline with `escurel admin migrate-kind-files` (no gateway).

Every command below was run against the real binaries on a throwaway store (a legacy tenant with two pages);
the transcript summary is at the end.

## How an orchestrator sees a quarantined tenant

`/healthz` is **200** (liveness is dependency-free) and `/readyz` is **200 too**, on purpose: the migration
needs a running server. What changed in this release is that readiness now **says** it:

```
HTTP/1.1 200 OK
x-escurel-quarantined: 1
{"ready":true,"notices":["quarantined", …],"components":{…,"quarantined":true,"migration_pending":false,…}}
```

and `/metrics` carries `escurel_tenant_quarantined{tenant="<t>"} 1` (it reads `0`, not absent, once lifted;
alert on `== 1`). **A proxy that gates only on the status code (kamal-proxy, a k8s readiness probe) will route
traffic to a quarantined tenant** that answers every call with `tenant_quarantined`. So the upgrade is
**stop-first with the migration BEFORE the traffic swap**, as below.

Every write or read surface refuses a quarantined tenant, not only MCP `tools/call`: `/ingest`, `/ingest/upload`,
`/blob/*` and `/ws` answer **503 `tenant_quarantined`** too (one shared check), so a quarantined tenant cannot
record events or serve a half-built index even if it is reachable.

### An interrupted migration is visible

`migrate-kind --apply` writes a **durable marker** (`meta/migrate-kind.pending` in the tenant's lane) *before* its
first rewrite and removes it only after pages, drafts, snapshots and the index rebuild have all succeeded. If the
process is killed in between, the next boot sees the marker and **quarantines the tenant again** (it is never
served on a half-migrated lane), `/readyz` carries `"migration_pending": true` and `/metrics` reads
`escurel_migration_pending 1`. Run `migrate-kind --apply` again: it is idempotent and clears the marker last.

Outbound connector and write-back health is on the same `/metrics` endpoint:
`escurel_egress_total{outcome}` (`ok`, `refused`, `limited`, `timeout`, `error`),
`escurel_write_back_total{outcome}` (`applied`, `conflict`, `failed`, `dead_letter`) and
`escurel_source_unavailable_total{kind}`.

## 0. Prerequisites

- **Where the CLI runs.** The image ships `escurel` (the 6.7 MiB operator CLI) next to `escurel-server`, so the
  migration runs *where the data is* with no admin API exposed (`docker exec`, `kubectl exec`, or the
  one-shot container in §3). A workstation with the `escurel` CLI, `--server <url>` and an admin bearer in
  `ESCUREL_TOKEN` also works if you already expose the admin surface.
- **Writers quiet.** The lane store has no compare-and-swap: the tool re-reads each page right before
  writing and reports one that moved as a conflict. Stop the runner and any integration that writes.
- **No live editing sessions** (the tool refuses `--apply` while a page has CRDT ops newer than its newest
  snapshot): close them, or run `escurel admin compact-lanes --tenant <t>` first.

## 1. Back up (the REAL restore point)

`escurel admin tenant export` is a logical export and it is **refused once the tenant is quarantined**
(`tenant_quarantined` — verified), so it cannot be your restore point *after* the new engine is up. The
restore point is the tenant directory, taken **with the server stopped** (DuckDB is single-writer; the
migration also rewrites the index, open drafts and CRDT snapshots, which only the whole directory holds):

```sh
# server STOPPED (Kamal: kamal app stop · k8s pet: scale the Deployment to 0)
tar -C /data -czf /backup/<tenant>-pre-kind.tgz tenants/<tenant>
```

(or the substrate's Volume snapshot / restic backup of `/data`). Optionally also take the logical export
**with the old engine, before upgrading**: `escurel admin tenant export --id <t> --out <t>.tgz`.

## 2. Dry run

Boot the new image once (it comes up quarantined; keep it **out of rotation**) and read the report. The dry
run is the default and writes nothing:

```sh
escurel --server http://127.0.0.1:8080 admin migrate-kind --tenant <t>
```

`pages_to_migrate`, `drafts` (open drafts that will be rewritten; their `content_sha256` changes),
`snapshots_to_rewrite`, `run_status_renamed`, and the things that need a human:

- `conflicts`: a page with BOTH a legacy `type:` and a `kind:` (usually its own `kind:` data field). Rename the
  data field in that page, then rerun. The tool never auto-fixes these.
- `skipped_pack_base`: signed pack pages (`markdown/base/**`) cannot be rewritten locally; the pack publisher
  must re-export and re-sign with `kind:`, then `escurel admin pack rebase`. The tenant stays quarantined
  until they are gone.
- `crdt_pages_with_live_ops`: close the session / compact the lanes.

## 3. Apply

In place, in the running container (replace the container name):

```sh
docker exec <container> escurel --server http://127.0.0.1:8080 admin migrate-kind --tenant <t> --apply
# k8s: kubectl exec deploy/<name> -- escurel --server http://127.0.0.1:8080 admin migrate-kind --tenant <t> --apply
```

**Or as a one-shot job BEFORE the swap** (recommended for Kamal/k8s: nothing ever routes to a quarantined
server, and no admin token is needed because the throwaway server listens on the container's loopback only).
Use the **same** env as production for the embedder (`ESCUREL_EMBEDDING_PROVIDER`, `ESCUREL_GEMINI_API_KEY`,
…) because `--apply` rebuilds the index with it:

```sh
docker run --rm -v <data-volume>:/data \
  -e ESCUREL_SERVER_LISTEN_HTTP=127.0.0.1:8080 -e ESCUREL_OBSERVABILITY_METRICS_LISTEN= \
  -e ESCUREL_TENANT=<t> -e ESCUREL_EMBEDDING_PROVIDER=... \
  --entrypoint sh <new-image> -c '
    escurel-server & S=$!
    until curl -fsS http://127.0.0.1:8080/healthz >/dev/null; do sleep 1; done
    escurel --server http://127.0.0.1:8080 admin migrate-kind --tenant "$ESCUREL_TENANT" --apply
    kill $S; wait $S'
```

(the one-shot script was run end to end against the real binaries: `"applied": true`,
`"tenant_quarantined": false`, the server exited 0 on SIGTERM.) In k8s run the same container as a `Job` that
mounts the data PVC, then roll the Deployment.

`--apply` is idempotent (a second run reports nothing to migrate), records an `escurel:kind-migration` system
event (the audit trail for rewritten drafts) and returns its id.

**How long it takes.** Reading the lane is O(pages) (it is also paid at every boot); the rewrite is quick
(3 pages: 0.14 s). `--apply` then **rebuilds the whole index**: with the zero-vector embedder that is seconds,
with a real embedder it **re-embeds the entire corpus** (the repo's own measurement: ~16 minutes for a
production-sized corpus, budgeted at 29). Plan the window for the rebuild, not for the rewrite.

## 4. Verify, then swap

```sh
curl -si http://<host>:8080/readyz     # 200 and NO x-escurel-quarantined header, "quarantined": false
curl -s  http://<host>:9090/metrics | grep escurel_tenant_quarantined   # … 0
```

The apply response says `tenant_quarantined: false`; if it says `true`, something legacy is left (a conflict
or a signed pack page): fix it (or have the publisher re-export) and rerun. There is no environment switch
that accepts the old key. A restart on migrated data is clean (verified: boots without notices).

Only now put the new container in rotation (Kamal: `kamal deploy`/proxy on; k8s: roll the Deployment).

## 5. Consumers

Every writer must emit `kind:` (templates, scaffolds, agent prompts and skills) and send `page_kind` to
`search`. An un-updated agent's writes are refused with `frontmatter_type_removed` (and a `suggestion`). See
the consumer checklist in `.claude/skills/escurel-platform/CHANGELOG.md` (0.8.0) and the repo
[`CHANGELOG.md`](../../CHANGELOG.md).

## Rollback

The migration changes pages, open drafts, historical snapshots and the index, in place. Rolling back is
**restoring the directory from step 1 and booting the previous image** (verified: after restoring the tarball
the new engine boots quarantined again, i.e. the legacy `type:` pages are back):

```sh
# server STOPPED
mv /data/tenants/<t> /data/tenants/<t>.migrated      # keep it for forensics
tar -C /data -xzf /backup/<t>-pre-kind.tgz
# deploy the previous image
```

Rewriting `kind:` back to `type:` in place is not supported.

## Transcript (throwaway store, real binaries)

```
backup:       tar -C data -czf backup-default.tgz tenants/default        -> 51864 bytes (server stopped)
boot new:     /readyz 200 + x-escurel-quarantined: 1, notices ["quarantined", …]; /metrics …{tenant="default"} 1
export:       escurel admin tenant export --id default …  -> tenant_quarantined (refused while quarantined)
dry run:      pages_to_migrate [instances/customer/acme.md, skills/customer.md], tenant_quarantined true  (nothing written)
apply:        "applied": true, "tenant_quarantined": false, audit_event_id 01M42HE4…   (0.139 s, 3 pages, zero embedder)
after:        /readyz 200, no header; …{tenant="default"} 0; restart: clean
rollback:     restore tarball, boot -> x-escurel-quarantined: 1 again; the page has `type:` again
```
