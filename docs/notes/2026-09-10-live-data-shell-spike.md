# Design spike — the live data shell (`sandbox_query`)

**Status: DRAFT for the escurel owner's decision. This is the "design spike
first" gate of async-ops Phase 6 — no code is proposed for merge until the owner
signs off on a mechanism (§7). It then becomes ADR-0013.**

Author: fleet async-ops work (epic #801). Date: 2026-09-10.
Related: `docs/adr/0012-multi-tenant-runner-isolation.md`, the async-ops concept
(scratchpad `async-ops-concept.md`), the fleet two-tier data-shell design
(`project_optimization_fleet`).

---

## 1. What the owner asked for

A **live, ad-hoc data shell**: an agent (or, through it, a caller) issues a
bounded SELECT and gets **current** rows back sub-second — the Tier-2 escape
hatch beneath curator-authored query pages, exposed as a peer tool
`sandbox_query` (explicitly **not** `bash`/`shell`). Owner constraints carried
from the concept review:

- reads must be **live / current** (not a stale nightly export), and sub-second;
- **default-on** for every fleet agent (so strict fail-boot entitlement parsing
  is a hard prerequisite — already in P0);
- ad-hoc SQL: scratch temp tables, joins across the caller's entitled views, and
  anofox-scenario what-if branches.

## 2. The constraint that kills the obvious design

escurel's read authorization is **not** in the SQL engine. `may_read_instance`
and the `vw_` view ACLs are **Rust predicates applied before a query is built** —
`materialise_view_on` / `prepare_source` in
`crates/escurel-index/src/backend/sql_view.rs` decide what source expression a
query is even allowed to reference, then hand a *narrowed* statement to DuckDB.

Therefore **any session that can execute arbitrary SQL on escurel's own gateway
connection bypasses the entire ACL layer**: it can `SELECT` from tables the
predicates would have hidden, read a **registered object-store secret**, and
reach **httpfs** to exfiltrate. The crew review of the concept rejected every
in-engine approach (Quack/`httpserver` *on the gateway connection*) for exactly
this reason. **This is the load-bearing finding of the spike: the data shell
cannot be escurel's live gateway connection.** It must be a *separate* execution
context that only ever sees data the ACL layer has already filtered.

## 3. Candidate mechanisms

### A — Per-session locked-down connection over pre-ACL-filtered views

A fresh DuckDB connection **per sandbox session**, opened hardened:

- `SET enable_external_access = false;` (no httpfs, no local file reads/writes),
- `SET allowed_directories = []` / a single scratch dir only,
- **no registered secrets** on this connection (so even if external access
  leaked, there is no credential to use),
- `SET lock_configuration = true;` **last**, so the session cannot re-enable any
  of the above.

Into that connection escurel installs **only the caller's ACL-filtered views** —
the same `vw_` narrowing `prepare_source` already computes, re-derived each turn
from the caller's entitlements — as the *only* attached, readable objects. The
agent reaches it via **Quack client mode** (DuckDB's client-server RPC, present
in v1.5.5). Arbitrary SQL is then safe *because the only data in scope is
already filtered* and the escape hatches (secrets, httpfs, filesystem) are locked
off, not merely unadvertised.

- **Live?** Yes — the filtered views read through to current catalog/parquet.
- **Isolation:** by attach-set (a session sees only its own filtered views) +
  the config lock. No caller can name another caller's data because it was never
  attached.
- **Cost/risk:** the hard part is guaranteeing the filtered views are the *sole*
  reachable objects and that `lock_configuration` truly closes every re-enable
  path on the target DuckDB version — this needs an adversarial test (try to
  reach a secret / httpfs / an unattached table from inside the session and prove
  each fails). Quack client-mode maturity in v1.5.5 (beta) is a second unknown.

### B — Continuously-refreshed read replica

A separate store that escurel refreshes on a cadence (export the caller-entitled
views to a replica the sandbox queries). Isolation is trivial (the replica holds
only exportable, already-filtered data; no secret, no httpfs). **But it relaxes
"live"** to the refresh interval — which the owner's requirement resists. Keep as
the fallback if A cannot be made both safe and live.

### C — In-engine on the gateway connection — REJECTED (§2)

Documented only so it is not re-proposed: it bypasses the ACL predicates and
reaches secrets/httpfs. Not viable at any freshness.

## 4. The freshness question the spike must resolve

Can **live** be satisfied **off-gateway**? Candidate A says yes *if* a
locked-down connection can read the current catalog/parquet through the filtered
views without its own object-store secret — i.e. whether the filtered view can be
materialised into the sandbox connection **by escurel** (which holds the secret
on *its* connection) rather than read **by** the sandbox (which must not). That
hinges on whether the narrowed source can be pushed as already-resolved data /
a view escurel populates, vs. a live passthrough the sandbox evaluates itself. If
only a passthrough gives sub-second live reads, and a passthrough needs the
secret on the sandbox connection, then A collapses toward B and the freshness bar
must relax. **Resolving this for the target DuckDB + Quack version is the spike's
central experiment**, and needs the escurel owner.

## 5. Bounds & runaway control (independent of A vs B)

- **Time:** a per-query statement timeout; a runaway query is **killed** and must
  not stall the tenant (the gateway's single-writer connection must be untouched
  by a sandbox stall — a second reason the sandbox is off-gateway).
- **Rows:** a hard row cap on returned results.
- **Isolation-by-attach:** the session's attach-set is re-derived each turn from
  the caller's live entitlements; nothing persists across callers.

## 6. DoD (no-mock, restated from the plan)

A bounded ad-hoc `SELECT` returns **current** rows the caller is entitled to; it
**cannot** reach (a) another caller's data, (b) a registered secret, or (c) the
filesystem/httpfs; a runaway query is killed and does not stall the tenant. The
security half must be an **adversarial** test — inside a live sandbox session,
attempt each of (a)/(b)/(c) and assert each fails closed.

## 7. Recommendation & the decisions the owner must make

**Recommendation:** pursue **A** (locked-down per-session connection over
pre-ACL-filtered views, reached via Quack client mode), with **B** as the
fallback the moment the §4 freshness experiment shows A cannot be both safe and
live. Do **not** write `sandbox_query` until §4 is answered on the real
DuckDB/Quack version.

**Owner decisions required before any code (this is the gate):**

1. **Mechanism:** A (locked-down connection + Quack) vs. B (refreshed replica) —
   pending the §4 experiment. Who runs that experiment (escurel owner + fleet)?
2. **Freshness bar:** if A cannot give sub-second *live* reads safely, is a
   bounded staleness (B) acceptable, or is the tool deferred rather than relaxed?
3. **Quack dependency:** is taking a beta Quack client-mode dependency in v1.5.5
   acceptable, or wait for its stable (~v2.0)?
4. **Where the sandbox process lives:** in-gateway-process second connection
   (still off the *writer* connection) vs. a separate sandbox sidecar — and how it
   authenticates to escurel to fetch the per-caller filtered attach-set.

On sign-off, this note becomes **ADR-0013** and Phase 6 implementation begins
against the chosen mechanism.
