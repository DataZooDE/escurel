# Changelog

All notable changes to escurel are recorded here. The format is
loosely [Keep a Changelog](https://keepachangelog.com/). Through the
`v1.0.x` line escurel used SemVer; from `v2026.07.13` onward versions
follow **`vYYYY.MM.DD`** — the date the binary set was cut (same-day
re-cuts append `.N`), matching the DataZoo release scheme (cf. erpl).


## Unreleased

## v2026.11.25

Added:
- **Separation of duties by person: the `same_person` claim (#876).** One human may reach a tenant under several subjects
  (one per identity provider). An issuer that knows this names the other subjects in the person's token
  (`same_person: ["<subject>", ...]`, at most 32 strings of 1 to 256 bytes without control characters; anything else
  is an invalid token, on every path). With the claim present (even empty) `promote_draft` and `promote_changeset`
  refuse a draft written by the caller's subject or any listed one (`self_review`); an admin with the claim that names
  the author itself in `decided_by` is refused too (a vouched human's other subjects are known only from that human's
  own token). `discard_draft` / `discard_changeset` of another person's draft, and superseding another person's stale
  draft in `create_draft`, need an admin or a caller that may write the target page as it stands (or, while there is
  none, the page the draft creates) (`not_author`; under `ESCUREL_WRITE_ACL=off` or `log` every caller may); the
  person's own drafts under any listed subject are always visible to them and can be withdrawn. The claim separates
  draft review only: a reviewer's correction (`content` on `promote_draft`) is the reviewer's own write, a draft a
  person's workbench agent filed is the person's (another person reviews it), branch merges are not covered, and an
  admin token without the claim may still vouch (`decided_by`) as before. Without the claim review is unchanged. A
  machine token (run, agent, actor, narrowed skill) never carries a person: the claim is ignored there. `whoami`
  reports `same_person`.

**Upgrade:** move the gateway and the runner together; nothing to set. A token issuer that starts minting
`same_person` turns on separation of review by person for those tokens: a person can no longer promote their own
drafts, and a person who may not write a page can no longer reject another person's draft of it.

## v2026.11.24

Added:
- **A skill can keep its instances out of default search (#873).** A skill page may declare
  `search: {hidden: true, exact: [<field>, ...]}`. A `search` that names no `skill` and no `page_id` then leaves the
  skill's instances out of every lane (native, SQL-view, rows) before the top-k cut, so many small records no longer
  crowd free-text results; the skill page itself stays searchable. An exact lookup still finds one: a query that
  equals (trimmed, case-insensitive) an instance's page id, slug / `id`, or the scalar value of an `exact` field
  returns it first, through the same read ACL and `page_kind` / `filter` / `as_of` / `scenario` constraints.
  `skill:` searches the instances as before. The exact lookup reads stored pages, so a hidden `instances: rows`
  skill is reached only by `skill:` (warning `search_hidden_rows`). A malformed block is refused on write with
  `search_invalid`; an undeclared `exact` field is the warning `search_exact_unknown`. `list_skills` publishes `search_hidden` /
  `search_exact`.

Fixed:
- **`diff_draft` reports a create with the empty-base sentinel as current while its page does not exist (#872).** A draft
  filed with `base_sha256: ""` (the create sentinel) was compared with the absent head and answered
  `base_moved: true`, so a review UI marked every such create out of date although nothing had moved. It is
  `false` now until the page appears, and `true` once it exists (the create can then never promote). Promotion and
  the change-set check use the same rule; with a CRDT backend a sentinel create whose page is still absent is now promoted
  with the create CAS rather than a merge against its recorded `base_version`; the outcome is the same.
- **A page seeded by the test harness is in lexical search (#874).** `EscurelProcess` (and so `escurel-test-gateway`)
  writes its fixtures after boot, and the BM25 index was built only at boot (the harness keeps the #868 refresh timer
  off), so seeded pages were found by the vector arm alone and a free-text search over a seeded tenant ranked by
  embedding only. The seed now builds the index once after the last page, as a server does for pages stored before
  boot. Test support only; the server is unchanged.

**Upgrade:** move the gateway and the runner together; nothing to set. A skill page whose `search` key is not
`{hidden: <bool>, exact: [<field>, ...]}` is now refused on write (`search_invalid`): check skill pages that already
carry a `search` key before upgrading.

## v2026.11.23

Changed:
- **Default `search` leaves escurel's system skills out (#845).** `escurel`, `escurel-agent`, `escurel-runtime` and
  `escurel-chat-channel` are seeded into every tenant and outranked tenant content: for "acme widgets" they took
  ranks 1–4 and the only page mentioning Acme was not in the top 5. A search that names no `skill` and no `page_id`
  now excludes them before the top-k cut. `skill:` (with `page_kind: skill` for a skill page) brings one back;
  `resolve`, `expand` and `list_skills` always reach them.
- **A sql_view skill page with `search_text` / `project` / `projection_limit` beside `backend:` is refused (#861)**
  with `misplaced_backend_key`. The binding reads them only inside `backend:`, so they were ignored without a word
  (this kept the lab's SQL search empty). A projection refresh reports views without search columns under
  `skipped`, and the boot log counts `skipped_no_columns` apart from `captured`.
- **A tenant skill id under the reserved `escurel-` prefix is refused (#857)** with `skill_id_reserved`, for an
  admin too: consumers hide every id under it as a system skill.

Fixed:
- **A page written while serving reaches lexical search (#868).** DuckDB's full-text index has no incremental
  update, and only boot, rebuild and the pack tools rebuilt it, so a page written with `update_page` stayed out of
  the BM25 arm until the next restart: everything an agent wrote during a pod's life was found by the vector arm
  alone, and the boot-seeded system skills won the lexical arm (#845). The server now rebuilds the index every
  `ESCUREL_FTS_REFRESH_SECS` (default 30; `0` = boot and rebuild only) when pages changed since the last build
  (~1 ms per page, under the tenant lock).
- **`%` and `_` in a `search` query are literal in the SQL lane (#856).** Since #849 a view's search columns are
  matched as one U+001F-joined text, so an unescaped `a%b` (or `a_b`) also matched a row whose `title` is `a` and
  `note` is `b`.

**Upgrade:** move the gateway and the runner together. New optional env `ESCUREL_FTS_REFRESH_SECS` (default 30).
A sql_view skill page with `search_text` / `project` /
`projection_limit` beside `backend:`, or a tenant skill id starting with `escurel-`, is now refused on write: move
the keys under `backend:` / rename the skill first.

## v2026.11.22

Fixed:
- **An event read no longer holds the tenant's connection (#858).** `list_events`, `list_inbox` and the paged event
  listings ran their query under the tenant's one connection lock. On a lake or attached-Postgres events backend that
  read is a remote round-trip per catalog lookup (~1.4 s in the lab), and the async runner polls every 5 s, so every
  other tool (`search`, `expand`, `query_instance`) queued behind it. Event reads now run on a clone of the connection:
  the lock is held only for the clone, and a read still sees every write committed before it.
- **List tools decide ACL once per page (#858).** `list_instances` (stored pages, `rows` and remote `rows` skills) took
  the connection three or more times per item for a non-admin caller; it now decides the page against one ACL snapshot
  (`Indexer::readable_mask`), as `search` does since #859.
- **A sql_view projection capture has a budget, and the boot warm-up captures views concurrently (#855).** A capture
  scans its whole source; a hung S3 or database read pinned a worker and delayed every later view. One past
  `ESCUREL_SQL_VIEW_CAPTURE_TIMEOUT_SECS` (default 300) is interrupted and rolled back, keeps the previous projection,
  and is reported under `failed`. The warm-up captures 3 views at once; captures of the same view stay serialised.

**Upgrade:** move the gateway and the runner together. New optional env `ESCUREL_SQL_VIEW_CAPTURE_TIMEOUT_SECS`
(default 300); nothing to set.

## v2026.11.21.1

Security:
- **An authored query page reads only its target (#854).** `query_instance` checked that the caller may read the
  target instance and then ran the page's SQL on the indexer's own connection, which also holds
  `external_credentials`. A caller who may write a `skill: query` instance and read any one sql_view instance could
  therefore read registered source credentials (plaintext DSNs), other instances' `vw_` views (including
  owner-private ones), `pages`, the search projection, and local or remote files through DuckDB's readers. The
  statement is now checked against DuckDB's own parse tree before it is prepared, default-deny: one `SELECT` that
  reads only the target view, its own CTEs and the `range` / `generate_series` / `unnest` generators, with no
  state-reading function. Anything else is refused with `sql_not_allowed` and the reason. Every lab report query
  passes unchanged; a query that joins a second table, uses `PIVOT` or reads a file now fails and must be rewritten
  over its target. **Upgrade:** move the gateway and the runner together, as for v2026.11.21.

Added:
- **`ESCUREL_RUNNER_DISPATCH=agent`: the runner dispatches only work an agent profile is declared for.** The runner
  dispatched every unprocessed `user` event in the inbox, so an event meant as a record (an audit entry a client
  writes after a person's decision, a receipt) became a model run that "folded" it into its page; one client wrote
  such a record per accepted draft and got one run per draft, plus drafts nobody asked for. With `agent` the runner
  takes an event only when its skill names a default `agent:` or the event asks for an agent that lists the skill;
  chat turns, workflow steps and manual starts are dispatched as before. Every other event stays in the inbox,
  unclaimed and unprocessed, for its own consumer (`/debug/admission`: `no_agent`). The default `all` keeps today's
  behaviour (#864). Test: `crates/escurel-runner/tests/suite/dispatch_policy.rs`.

Fixed:
- **A requeue of an event that is already processed runs nothing.** `/dlq/requeue` and the `requeue` / `retry`
  controls re-drove a dead-lettered run even when the event's consumer had filed it meanwhile (`assign_event`), so
  the run repeated an effect that had already happened. The runner now settles the row `processed` and answers
  `{requeued: false, reason: "event_processed"}` (controls: outcome `not_run`). Under `ESCUREL_RUNNER_DISPATCH=agent`
  a requeue of an event no agent is declared for is not run either (`no_agent`) (#864).
- **A member of `ESCUREL_SKILL_ADMIN_GROUPS` accepts a changeset that adds skills.** The skill-page rule (#847) let
  the named groups write skill pages, but the skill-authority gate (v2026.11.13) still granted authority keys
  (`acl`, `autonomy`, …) only to `escurel:admin` or the stored page's `acl.update`. A new skill has no stored page,
  so promoting its create draft was refused with `-32003 authority_keys_need_admin` while the changes of existing
  skills in the same release landed. A person in a named group now changes authority keys as an admin does (also on
  an existing skill whose `acl.update` does not name the group). Machine callers are still never granted, and `*`
  grants no authority keys (#862).
- **An unfiltered `search` no longer takes the tenant's connection once per skill.** The rows lane and the
  "not searched: REST/MCP-backed skills" hint re-read every skill page (`rows_source` / `remote_rows::source`) under
  the connection lock, and the SQL lane took it three times per `sql_view` instance. Behind a busy neighbour (in the
  lab the async runner's `list_events` holds it ~1.4 s per call) each acquisition queued, so a search over 56 skills
  did not return in minutes while a single-skill one took ~7 s. The lanes now decide from one `list_skills` read
  and count every `sql_view` match under one lock (#853).
- **A non-admin `search` no longer takes the connection three or more times per hit.** The fail-closed ACL filter
  looked up the hit's skill ACL, the tenant defaults and the caller's groups for every hit, per lane, per query
  variant — the path every agent and end-user token takes (the lab measurements behind #853 used an admin token,
  which bypasses it). A search now reads the catalogue and the caller's ACL snapshot once (`Indexer::acl_context`,
  `may_read_instance_in`) and shares them across lanes and variants; an `owner_field` owner is resolved only when
  the policy names `owner`, and once per request. Decisions are unchanged; a policy edit applies from the next
  request rather than mid-search.
- **The SQL lane's projection table is part of the schema.** `sql_view_search_text` (with a `(view, sig)` index) is
  created by `Migrator` on every open instead of lazily by the first capture, whose DDL could race across
  connections. A projection read error is logged instead of reading as "no match", and
  `refresh_search_projections` returns a per-view report (`ProjectionRefresh`: captured / failed).

- **A sql_view instance cannot silently drop out of search any more.** A projection refresh that read an overlay
  before a re-create and published after it overwrote the new projection with the old key, and the instance was not
  found until a restart. Captures are now serialised, and each one derives its key from the overlay's and skill's
  CURRENT state, so the last capture always matches the live binding. Editing a sql_view skill page re-captures its
  instances (a `search_text` change used to empty the SQL lane for that skill). Retracting an instance
  (`delete_page`) removes its cached search text, and a full refresh prunes rows no overlay names.

Corrections to #849's entry (v2026.11.21): match counts are not strictly unchanged. Columns are matched as one
`chr(31)`-joined text, so an unescaped `%`/`_` in `q` can match across two columns, rows whose search columns are
all NULL or empty are not counted, and beyond 100 000 distinct texts per view counts are lower bounds.

## v2026.11.21 — BREAKING (system skills are namespaced: `escurel-agent`, `escurel-runtime`, `escurel-chat-channel`)

#850.

The system skills escurel ships were `agent`, `runtime` and `chat-channel`. Those are names tenants use for
their own skills, and the admin-only rule followed the name: a portfolio's own `agent` skill and everything under
`markdown/instances/agent/` became admin-write-only. They are now `escurel-agent`, `escurel-runtime` and
`escurel-chat-channel`; the `escurel-` prefix is reserved for escurel. A tenant's own `agent`, `runtime` or
`chat-channel` skill is an ordinary skill again.

**Upgrade checklist:**
1. Nothing to do for the pages themselves. On boot, a legacy `markdown/skills/agent.md` (`runtime.md`,
   `chat-channel.md`) that is byte-equal to a version escurel shipped is escurel's: it is removed and its
   instances move to `markdown/instances/escurel-agent/` (`escurel-runtime/`, `escurel-chat-channel/`) with
   `skill:` rewritten in place; the namespaced skill page is installed. The log line is "migrated a system skill
   to its namespaced id". A legacy page with any other content is the tenant's own skill and is never touched
   ("a tenant skill carries the id …"). If you had EDITED escurel's agent page, it stays as a tenant skill
   called `agent`: move its profiles by hand (rename to `markdown/instances/escurel-agent/<id>.md`, set
   `skill: escurel-agent`) and delete the page.
2. Upgrade runners and the gateway together: a runner registers under `escurel-runtime`, and an older runner's
   `runtime` page is no longer the runner's to write. The VS Code extension (agents view, "Map Teams
   conversation…") moves with this release.
3. A skill's `agent: "[[agent::x]]"` binding keeps working (that key only names agent profiles; it is read as
   `[[escurel-agent::x]]`, also by `validate` on a tenant without its own `agent` skill). Write the new form
   in new pages.

Fixed:
- **A system skill is seeded once.** The seed wrote each system page twice: once directly, once through the
  ordinary write path. On a Google Drive lane the second write could create a second file with the same name
  (heron-escurel, 2026-11-18: two identical `markdown/skills/agent.md` three seconds apart), and every later
  boot failed with "is ambiguous: 2 files share that name". The seed now decides against the lane, creates with
  the new `LaneStore::create_if_absent` (atomic on the filesystem store: hard link; read-then-write elsewhere,
  where S3 and GCS replace by key anyway) and indexes without a second write. Repeated boots on fresh indexes
  and two gateways seeding one lane at once store one copy (tested).
- **A Drive lane tolerates identical duplicates.** Reading a name that two or more Drive files share, all with
  the same bytes, returns those bytes and warns instead of failing; writing it first removes all but one (lowest
  file id); deleting it removes all. Duplicates with different bytes stay an error (which one is the page is not
  escurel's to guess). This lets heron-escurel boot without a manual clean-up of its duplicate `agent.md`.


Fixed:
- **`search` no longer scans every `sql_view` source on every call.** The SQL lane matched `q` with
  `SELECT count(*) FROM vw_… WHERE col ILIKE ?` per instance, per query variant, on the tenant's one connection — a
  full read of each external source (parquet on object storage in production) per search. In the lab an unfiltered
  search took more than 90 s, and every other tool queued behind those scans. The lane now reads a local projection
  of each view's `search_text` values (distinct texts with their row counts, capped at 100 000 per view), captured on
  a cloned connection by `create_instance`, `rebuild`, and in the background after a writer boot. Match counts and
  ranking are unchanged. **Behaviour change:** a change to a source becomes searchable after the next capture
  (re-create the instance, rebuild, or restart), not immediately; `Indexer::refresh_search_projections` re-captures
  on demand. Reading an instance (`query_instance`, projections) still reads the source live.

## v2026.11.20 — BREAKING (skill pages are written by administrators)

Writing a skill page and promoting a draft of one need `escurel:admin` or a group named in
`ESCUREL_SKILL_ADMIN_GROUPS` (#847).

**Upgrade checklist:**
1. Writing a skill page, and promoting a draft of one, now needs `escurel:admin` unless you name more groups in
   `ESCUREL_SKILL_ADMIN_GROUPS`. A deployment whose members author skills directly sets it to those members'
   group, or to `*` for the earlier behaviour (every writer the rest of the write ACL allows).
2. Nothing changes under `ESCUREL_WRITE_ACL=off`; under `log` the would-be refusals are logged.

Added:
- **Skill-page write rule (`ESCUREL_SKILL_ADMIN_GROUPS`).** A skill page (`markdown/skills/*`, or any page whose stored
  or incoming frontmatter says `kind: skill`) is written only by `escurel:admin` or a person in one of the named groups
  (token groups or `group_members`; a run or agent token is never granted by a group). Every write door applies it:
  `update_page` (branch writes included), `promote_draft` (with or without corrected content), `promote_changeset`
  and `merge_branch` (refused as a whole before any member lands), `delete_page`, `move_page`, `open_session` and
  the `close_session` commit. Proposing stays open: whoever could write the page before may still `create_draft`
  it, so authors propose and administrators promote. The refusal is an `ok: false` issue with code
  `skill_page_needs_admin` naming the page and who may write it. Instance pages keep their own ACL. Boot fails on
  `*` mixed with group names or on a structural group (`public`, `owner`, `admin`). `escurel-test-gateway` reads the
  variable under `--write-acl-enforce`.

## v2026.11.18

A runner-only patch release (#844), found when heron-escurel moved to v2026.11.17. No upgrade steps; only the runner
image changes behaviour, so a deployment can re-pin its runners and leave the gateway on v2026.11.17.

Fixed:
- **The runner no longer turns a `page_write` notification into a run.** With `ESCUREL_WEBHOOK_URL` pointed at the
  runner's `/trigger`, the gateway also posts a `page_write` notification after each instance write. It names no
  event, but the runner parsed it as one and dispatched a run for the written page's skill that it could neither
  claim nor deduplicate. On heron-escurel the runner's own `runtime` page write dead-lettered a run for a skill
  `runtime` that does not exist. `/trigger` now acknowledges a body without an `event_id` (202) and takes nothing.

## v2026.11.17

A patch release with two fixes from the codex review of v2026.11.14. No upgrade steps.

Fixed:

- An agent whose `scopes.tools` leave out `assign_event` again has its event filed after its write lands. Since
  v2026.11.14 the runner filed the event on the run's own token, so the agent's tool scope refused it with
  `agent_scope_denied`; the event stayed unprocessed and was retried for a write that had already landed. The
  runner files it on its own credential again; the instance is still read back with the run's token.
- VS Code "Map Teams conversation…" writes the group as a YAML string, so a valid group named like a number,
  a boolean or `null` (`123`, `true`) is no longer a mapping the gateway refuses.

Also: the S2D demo's supplier-risk report renders its chart from an anofox role spec through peacock (#838,
demo only), the pre-push hook runs the Rust gate only when a push touches Rust (#837), and a runner test waits
for the ledger's real `dead_letter` status (#840).

## v2026.11.14

Runners outside the cluster without admin, revocable tokens for external agents, and a Teams bridge that holds no
signing key, reads directory groups (Microsoft Graph, Google, Okta), answers group chats and channels, and can be
verified by the gateway against the Teams roster (#827, #828, #829, #830, #835).

**Upgrade checklist:**
1. Optional: give out-of-cluster runners the narrow `escurel:runner` role instead of `escurel:admin`.
2. A runner on a static/file token now asks a signing gateway for each run's token, so its ordinary runs write as
   `agent:<skill>` (as a minting runner's do) instead of as the runner. Check ACLs or dashboards that match the
   runner's subject.
3. An agent token without an `iat` claim is refused.
4. Group directory and Teams URLs over plain http (loopback stand-ins only) now need `ESCUREL_EGRESS_ALLOW_LOOPBACK=true`,
   or the gateway refuses to boot, naming the key and the flag.
5. Chat bridge: move to gateway-minted turns (`ESCUREL_BRIDGE_GATEWAY_TOKEN[_FILE]` with `escurel:bridge`,
   `ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE` on the gateway), and for production also set the gateway's Teams
   verification (`ESCUREL_AUTH_TEAMS_APP_ID` / `_APP_SECRET`). See
   [`docs/deploy/chat-bridge.md`](docs/deploy/chat-bridge.md).

**Deprecated:** the chat bridge signing its own turns (`ESCUREL_BRIDGE_AUTH_ISSUER` / `_KID` / `_SIGNING_KEY` and
`ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE` on the bridge, `ESCUREL_AUTH_BRIDGE_ISSUER` / `_KID` / `_TENANT` on the gateway).
It still works for this release (1:1 chats, no groups) and logs a deprecation at both ends; it is removed in the
next release. Migrate with the keep/remove checklist in
[`docs/deploy/chat-bridge.md` § Migration checklist](docs/deploy/chat-bridge.md#migration-checklist).

Runners outside the cluster and agents outside escurel (#828, #829):

- **`escurel:runner` service role** (`ESCUREL_AUTH_RUNNER_ROLE_VALUE`): a runner no longer needs `escurel:admin`. The
  role reads the inbox, claims, keeps the runner's books and writes its own `runtime` page, and nothing else;
  `whoami` says `service`, `list_runtimes` shows each runner's `role`. A run token never inherits it. The
  `escurel:bridge` value (`ESCUREL_AUTH_BRIDGE_ROLE_VALUE`) is reserved the same way for the chat bridge (#828).
- **`mint_run_token`**: a runner with a static or file credential and no signing key gets each run's token from
  the gateway — the same claims a minting runner signs, narrowed to the skill's write groups or the agent profile,
  never the wide admin shape for a runner credential, and only for a run the caller itself started of that skill
  (as an agent that lists it). Agent-profile skills therefore run on such a runner; a fresh token is minted for
  every attempt, a failed mint fails the attempt (retryably) rather than running on the runner's own credential,
  and the run's write is read back with the run's token (#828).
- **Concurrent admission**: a poll claims up to `ESCUREL_RUNNER_ADMIT_CONCURRENCY` (default 8, 1..=64) inbox events at
  once, in inbox order (#828).
- **External agent tokens**: `issue_agent_token` / `escurel agent token issue <agent>` (admin) hands Claude Code,
  Codex or Multica a token that runs as an agent profile, confined by its scopes; `revoke_agent_tokens` /
  `escurel agent token revoke` (admin) moves the agent page's new `token_epoch` forward on the gateway's clock, and
  older tokens — external and run tokens — get `agent_token_revoked` from their next call (#829).
- The shipped `agent` system-skill page is upgraded in place when a tenant never edited it (it gains
  `token_epoch`); an edited page is kept as before.
- UI: VS Code Agents view "Issue token…" / "Revoke tokens", runner role in the Runtimes view; explore Agents panel
  shows "tokens valid since".

Runner gaps (#827):

- **Inject capability from the runtime.** `list_runtimes` `harnesses[].supports_inject` is read from the harness
  itself (claude, echo: `true`; codex and in-process harnesses: `false`; absent from an older runner = unknown). VS Code
  no longer guesses from the harness name: the Steer box follows the runtime holding the run, and says when it cannot
  tell (the runner still answers `unsupported`). The Runtimes view tooltip says which harnesses take a message.
- **Codex shell commands in the transcript.** A codex `command_execution` is a `command` transcript record (`<command>
  (exit <code>)`, redacted like every summary), not a tool call; VS Code's transcript tab shows and filters it.
- **Per-tenant transcript retention.** `tenant_update{transcript_retention_days}` /
  `escurel admin tenant update --transcript-retention-days <n>` wins over `ESCUREL_RUN_TRANSCRIPT_RETENTION_DAYS`
  (`0` = forever) and applies on the next sweep, without a restart.
- **Webhook-fired schedules.** With `ESCUREL_SCHEDULE_WEBHOOK_SECRET[_FILE]` set, a signed
  `POST /hooks/schedule/<skill>` fires a skill that opts in with `schedule_webhook: true` (a new authority key),
  captured as `hook:<skill>:<delivery>` (a redelivery is one event). The signature
  (`X-Escurel-Webhook-Signature`) covers `escurel-schedule-v1\n<skill>\n<delivery>\n<timestamp>\n<body>`, with
  `X-Escurel-Delivery` and `X-Escurel-Timestamp` (within 5 minutes), so a captured request cannot be replayed. Its
  deliveries share the skill's `schedule_budget` with the cron windows, counted under a per-skill lock; past it the
  day's first skip files a `run-skipped` record, and a skipped delivery stays skipped. `hook:` event ids are
  reserved like `sched:`.

Chat bridge: gateway-minted turns, directory groups, group chats (#830):

- **`mint_user_token`**: the Teams bridge holds no signing key. On its own `escurel:bridge` credential
  (`ESCUREL_BRIDGE_GATEWAY_TOKEN[_FILE]`) it gets each turn's token from the gateway, which decides the subject
  (`ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE`, moved from the bridge) and the groups.
- **Group directories**: `ESCUREL_AUTH_GROUP_DIRECTORIES=graph,google,okta` (any subset) — Microsoft Graph
  (`transitiveMemberOf`, by Entra object id), Google Cloud Identity (`searchTransitiveGroups`, service-account
  JWT-bearer, by email) and Okta (`users/{login}/groups`, SSWS token, by email or UPN). The token carries the union,
  mapped per provider by a prefix and/or a map file; reserved names are never produced. Cached 10 min, a stale
  answer used for up to 1 h while a directory is down, then one "can't check your access right now" reply. The
  bridge reads the sender's email / UPN from the Bot Framework conversation-members API. A 1:1 asker now reads what
  their directory groups may read; the chat run carries those groups (`captured_groups`).
- **Group chats and channels**: an @mention of the bot in a Teams group chat or channel thread is answered when an
  admin maps the conversation with a page of the new admin-only `chat-channel` system skill. The run acts as
  `group:<g>` with `<g>`'s access only, the asker recorded (`asked_by`, `act.sub`) and required to be a member of
  `<g>`. An unmapped chat or a non-member gets one polite reply and no run. Channel replies go to the thread root.
- **Safe group naming**: a directory prefix reads only names an admin controls — Graph security groups (Microsoft
  365 groups with `ESCUREL_AUTH_GRAPH_INCLUDE_M365_GROUPS=true`), Google groups by full address
  (`ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN` pins domains; `email_local` needs it), Okta `OKTA_GROUP`s
  (`ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS=true` for app groups). The id-keyed map file is the recommended mode.
- **Identity binding**: with Graph, the bridge's asserted email / UPN must be one of the sender's Graph addresses
  (`identity_mismatch`; needs `User.Read.All`), an unknown object id mints nothing, and a minted subject can never
  start with `group:` / `agent:` / `bridge:` / `svc:` / `escurel:`. A turn token is honoured only under the gateway's
  own signing identity, with every role value stripped.
- `ESCUREL_AUTH_GROUP_DIRECTORIES` is optional: without it 1:1 turns mint groupless and shared chats get one reply.
- The bridge's `ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE` is re-read on change and on a 401; a setup mistake replies once
  and logs the fix; a failed re-mint no longer drops a captured turn's answer.
- UI: VS Code Knowledge view lists `chat-channel` pages with a "Map Teams conversation" form (admin; one page per
  conversation, an existing mapping is opened instead of duplicated); explore's Chat tab labels a conversation that
  acts as a group.

Chat bridge hardening (#835):

- **Added — gateway-side Teams verification** (`ESCUREL_AUTH_TEAMS_APP_ID`, `…_APP_SECRET[_FILE]`, `…_OPENID_METADATA`,
  `…_ISSUER`, `…_TOKEN_URL`, `…_TOKEN_SCOPE`, `…_SERVICE_URL_HOSTS`): the bridge forwards each activity's Bot Framework
  bearer to `mint_user_token` (`activity_token`, `service_url`), and the gateway verifies it, asks the connector's
  roster whether the sender is in the conversation (1:1, group chat and channel), and uses Microsoft's object id,
  tenant, email and UPN. New codes `activity_unverified`, `not_in_conversation` (the bridge drops both silently) and
  `identity_mismatch` for a disagreeing bridge. The Bot Framework token check now lives once in
  `escurel_auth::bot_framework`, shared by the bridge and the gateway.
- **Added — mint rate limits**: `ESCUREL_AUTH_BRIDGE_MINT_PER_MINUTE` (600, per bridge credential, before any lookup)
  and `ESCUREL_AUTH_BRIDGE_MINT_PER_ASKER_PER_MINUTE` (30); `rate_limited` (retryable, `retry_after_secs`, `scope`).
  The bridge waits out a per-credential cap once when it lifts within 10 s; otherwise the person is told once.
  `escurel_client::Error::JsonRpc` carries the whole `error.data` as `data`.
- **Security**: with Teams verification on, a stolen `escurel:bridge` credential alone no longer mints for anyone,
  and forged identity fields (object id, tenant, email, UPN) are refused. It does NOT bind a mint to one
  conversation: the Bot Framework signs no activity body, so a captured, unexpired activity token plus a stolen
  bridge credential can mint for any member of any conversation the bot is in on that `serviceUrl`, until the
  token expires. The roster check stops forging people who are in no such conversation. One credential minting
  for more than 50 distinct askers in a minute is logged at WARN. Without verification the gateway warns once
  at boot that it trusts the bridge for who asked.
- The bridge drops a turn whose own member lookup answers `404` (the sender is not in the conversation) without a
  reply.
- With verification on, a conversation kind its Teams id contradicts (a `19:…@thread.v2` group chat called
  `personal`, a channel turn with no team) is `identity_mismatch`. The Bot Framework keys are fetched with the
  caller's client (the gateway's egress-vetted one), never through a redirect, and only from an https `jwks_uri`
  (plain http only to loopback behind the dev flag).
- **Added — Okta as an OAuth service app**: `ESCUREL_AUTH_OKTA_AUTH=oauth` with `ESCUREL_AUTH_OKTA_CLIENT_ID` and
  `ESCUREL_AUTH_OKTA_PRIVATE_KEY[_FILE]` (optional `…_KEY_ID`, `…_TOKEN_URL`, default `<base>/oauth2/v1/token`)
  reads the groups with a client-credentials token (`private_key_jwt`: a short-lived RS256 assertion, a fresh `jti`
  each time; scopes `okta.groups.read okta.users.read`), cached until shortly before it expires, instead of an
  admin's SSWS API token. `ssws` stays the default while `ESCUREL_AUTH_OKTA_API_TOKEN` is set; with neither the boot
  asks for one, and a key of the other mode is refused. A key that does not parse fails the boot. Okta's "Require
  DPoP" must be off on the app: escurel sends no DPoP proof, and a token endpoint asking for one is named in the
  gateway's log as the setting to turn off.
- **Changed:** a group directory's or Teams `…_BASE_URL` / `…_TOKEN_URL` may be plain http (to a loopback address)
  only with `ESCUREL_EGRESS_ALLOW_LOOPBACK=true`.

## v2026.11.13 — BREAKING (runner credential must be admin; skill authority keys need an admin)

The Multica-gap epic (#817): runs you can steer, read and bill, agents with least privilege, cron schedules,
distributed runtimes, SKILL.md interop and a Microsoft Teams bridge — plus the runner, embedding and VS Code work
merged since v2026.10.29.

**Upgrade checklist (in this order):**
1. Give every runner an admin-capable credential (`escurel:admin` on `ESCUREL_RUNNER_TOKEN` / `ESCUREL_RUNNER_TOKEN_FILE`,
   or minting mode) **before** the new image runs — otherwise it takes no inbox work (see *Breaking* below).
2. Give the people who curate skills `escurel:admin`, or list their group in each skill's `acl.update`.
3. If a skill page names `model:`, list that model in `ESCUREL_RUNNER_MODEL_ALLOW` (#810), or its runs fail closed.
4. **The first boot re-embeds the whole corpus once** (#815): an index created before this release has no recorded
   embedding model, and an unrecorded model counts as a change. It is staged per batch (old vectors keep serving; a
   provider failure resumes next boot), but it costs one full embedding pass at the provider's rate limit — schedule
   it. Changing provider or model later re-embeds again.
5. Dashboards that match the exact label set of `escurel_runner_cost_usd_total` / `escurel_runner_tokens_total`
   need the new `skill` and `model` labels.

Runs you can see, steer and bill; agents with least privilege; cron schedules (epic #817: #819–#823).

- **Steer** a live run with `run_control{action:"inject"}` (claude, echo; codex answers `unsupported`) (#820).
- **Run transcripts**, redacted and stored per run, read with `fetch_blob{run_id}`; retention via
  `ESCUREL_RUN_TRANSCRIPT_RETENTION_DAYS` (#822). New table `run_transcripts` (migration 0022).
- **`usage_report`** cost rollups and `list_events{since, until}`; CLI `escurel run usage` (#821).
- **Agent profiles** (`agent` system skill, admin-only): the gateway enforces `scopes.tools` and `scopes.write` on
  run tokens with an `agent` claim, plus per-agent concurrency (#823).
- **Schedules:** `schedule:` / `schedule_budget:` skill frontmatter, fired by the runner; the lint tick becomes a
  schedule (`ESCUREL_RUNNER_SCHEDULE_TICK`, `ESCUREL_RUNNER_SCHEDULE_GRACE`) (#819).
- UI: VS Code transcript tab, cost chip, Steer box, Usage and Agents views, schedule chips; explore cost badge,
  Agents panel and schedule badge.

**Behaviour changes to note:**
- Metrics: `escurel_runner_cost_usd_total` and `escurel_runner_tokens_total` gain `skill` and `model` labels —
  dashboards that sum without `by` are unaffected, ones that match the exact label set need updating.
- Writes: a page write must be allowed by the declared skill, the path's skill **and** the stored page's skill
  (relabels and moves can no longer escape the stored skill's ACL); `move_page` checks the destination's ACL.
- claude runs now use stream-json; `usage` sums every turn, and `tool_calls` counts `tool_use` blocks.
- Event ids starting `run:`, `sched:` or `lint-` are reserved for the runner's identity.
- `mint_agent_token` refuses machine callers (agent, narrowed, run and workbench tokens).
- `usage_report` totals carry `priced_runs` / `unpriced_runs`; the UIs say "cost unavailable" instead of $0.00.
- With several runners, only the runner that holds a run answers its cancel / retry / inject at once; the others defer.
- codex tool calls are read from `item.type` (codex-cli 0.168 shape) and counted once per completed call.

Distributed runtimes, SKILL.md interop and a Teams bridge (epic #817: #818, #824, #825).

- **Runtimes (#818):** runners register `runtime` pages and route by `requires: {harness, labels}`; gateway
  `claim_event` / `renew_claim` leases give each event one runner (with a boot grace after a gateway restart);
  `ESCUREL_RUNNER_EXCLUSIVE` for laptop/on-prem runners; `list_runtimes`, `escurel runtime list`. Scheduled windows
  run exactly once across runners; the holding runner answers run control.
- **SKILL.md (#825):** `escurel skill export --as skill-md` / `escurel skill import`, VS Code "Export as SKILL.md",
  references/12 on escurel as memory MCP for external agents.
- **Teams (#824):** new `escurel-chat-bridge` binary (in the runner image, off unless configured); see
  `docs/deploy/chat-bridge.md`. New gateway key `ESCUREL_AUTH_BRIDGE_KID` restricts what a bridge-signed token may do.
- UI: VS Code Runtimes view, `requires` chips, claim holder in run detail; explore Runtimes panel.

**Behaviour changes to note:** a third system skill (`runtime`) appears in `list_skills`; the gateway's page-chat
reply rule and the runner's chat rule accept `captured_via: bridge:<channel>`.

- **Breaking — every runner needs an admin credential.** A runner whose `claim_event` is refused for
  authorisation (`-32001` / `-32003` / `401` / `403`) now takes **no** inbox work, lone or in a fleet (the
  lone-runner exception is gone), and logs *the runner credential needs escurel:admin to claim events*; every
  runner checks `whoami` at boot and logs that error when it is not an admin (it keeps serving `/healthz`). A
  gateway without claim tools (`-32601`) and synthetic webhook events are unaffected. **Upgrade step:** give the
  runner an admin-capable credential (`escurel:admin` on `ESCUREL_RUNNER_TOKEN` / `_FILE`, or minting mode)
  before rolling out. A dev gateway without OIDC treats every caller as admin.
- **Breaking — skill authority keys need an admin.** A write that adds, removes or changes an authority key
  (`acl`, `agent`, `autonomy`, `schedule`, `requires`, `harness`, `model`, `cascade`, `actions`, `backend`,
  `freeze_when`, …; `escurel_types::skill_authority`) on a skill page lands only for an `escurel:admin` person or a member of the
  stored page's `acl.update` groups — on `update_page`, `promote_draft` / `promote_changeset` (the promoter is
  judged), the `close_session` commit, `delete_page` and `move_page`. A new skill that sets one is an admin's.
  Refusal: `-32003`, `data.code: authority_keys_need_admin`, `data.keys`. `ESCUREL_WRITE_ACL=log` logs and
  allows; `off` is ungated. Upgrade: give the people who curate skills `escurel:admin`, or list their group in
  each skill's `acl.update`.

Also in this release (merged since v2026.10.29):

- **Skill `model:`** picks the run's model within the new `ESCUREL_RUNNER_MODEL_ALLOW`; outside it the run fails
  closed (#810). **`ESCUREL_RUNNER_CHAT_MODEL`** sets the model for chat runs; the gemini turn bound is 24 (#813).
- **OpenAI-compatible runner harness** with `alibaba`, `hetzner` and `openai-compatible` presets (#814).
- **OpenAI-compatible embeddings** (`ESCUREL_EMBEDDING_PROVIDER`); the embedding model is part of the index
  identity, so a boot under another model re-embeds instead of mixing vector spaces (#815).
- **Lake GC** runs on its own connection in chunks, so reads never wait on it (#812).
- **`/ws` `event_assigned`**: an open Inbox drops an event once it is filed (#811).
- **VS Code:** a comparison's stored charts render inline, fetched and hash-verified by the host (#816).
- Docs: where ACL groups come from; `ESCUREL_AUTH_GROUPS_CLAIM` defaults to `roles` (#809).

## v2026.10.29

Replies reach the requester, and a service role may reply under a declared label.

- **Lineage read.** Under the event ACL, the subject that captured a root event reads every `user` event (reply) in that root's lineage
  (`root_event_id`), filed or not, whoever captured it. The requester of an objectless request now reads the service's
  unassigned reply; nobody else gains anything.
- **`acl.reply_into: [groups]`** on a skill: a role-restricted caller may capture a reply under that skill's label into a
  readable root of another label. Without it a restricted reply keeps the root's label. `list_skills` reports it to
  admins.
- Also in this release: the explore Chat tab in the right rail, answered by a governed run (#802, #803); an absent
  `autonomy:` means auto in the runner and `list_skills` as at the gateway (#804); cold chat latency notes for the
  claude and gemini harnesses (#805).

## v2026.10.28

A service-role gate for workers that run under a service identity, and page chat answered by the runner.

**Page chat** (#789, #791–#797, #799–#801, #806):
- The runner has a chat mode: it answers a page-chat turn as the asker and posts the reply; chat runs use their own
  lane and never wait behind the serial lane; failed, cancelled or unmintable chat runs say so; turns deduplicate on
  `(chat_group_id, user_msg_id)`.
- `chat_corpus()` with an owner-only conversation skill; `chat:` is validated.
- WebSocket chat access is rechecked per message; a timed-out chat run says so.
- VS Code: chat client methods, the `/ws` chat frame, a pure chat model and the Chat webview (conversation, prompt,
  Recent, New chat).
- VS Code: a Chat view beside Runs, answered live by a governed run; Ask about this record, chat focus and Review for
  proposed changes.
- `whoami`: the caller's verified subject, tenant and admin flag (also `escurel whoami`, `Client::whoami()`).

**Service-role gate:**

- **`ESCUREL_AUTH_ROLE_TOOLS`** (`role=tool,tool;role=tool`): a token whose groups carry a listed role may call only
  those tools (`-32001`, `role_restricted`); `tools/list` shows only them; `/ingest`, `/ingest/upload`, `/blob` and `/ws`
  answer `403`. A restricted `create_draft` needs `acl.propose`; a restricted `capture_event` must reply to a readable
  event under its label. Minted children inherit the gate. Boot fails on an unknown or admin tool.
- **Skill grants `acl.propose`, `acl.read_events`, `acl.read_referenced`**: drafts without write rights, every event of a
  label (and filing it into a readable instance), and instances a visible event references. `list_skills` reports them to
  admins.
- **`escurel-test-gateway`** honours `ESCUREL_EVENT_ACL` and `ESCUREL_AUTH_ROLE_TOOLS`, takes `--event-acl-enforce`, and
  prints a `service_bearer` for `--service-subject`/`--service-group`.

Design: `docs/design/service-role-gate.md`.

## v2026.10.27

A field rule that caps how many values a field holds, and chat-message pushes on the event subscription.

- **`event_subscribe` with a `chat_group_id` filter** pushes appended chat messages of that group (#788).
- **`max_items: <n>` on a field.** A skill may cap how many values a field holds, mainly for `kind: link` fields that take
  one link or a list: a list counts its entries, a scalar counts as one. A page over the cap is refused by `update_page`,
  `create_draft`/`validate` and `promote_draft` with `field_max_items` (family `field_max_items`). `n` must be a positive
  whole number; anything else refuses the skill page with `fields_malformed`. `list_skills` publishes `fields[].max_items`.

## v2026.10.26 — BREAKING (operator-only tenant administration, upload create ACL, `required_when` wire shape, web-workbench override names)

Skill-declared data rules, a public API for companion VS Code extensions, a web-workbench overlay contract, an operator
credential and stricter upload ACLs (#767–#786). Upgrade notes first, then the changes.

**BREAKING.**
- **Tenant administration is operator-only** (#780). Tokens whose roles carry `escurel:operator` (configurable with
  `ESCUREL_AUTH_OPERATOR_ROLE_VALUE`) may use `tenant_create`/`list`/`get`/`update`/`delete` and export or import another
  tenant; a tenant-scoped admin acts on its own tenant only. Existing admin tokens keep working for their own tenant.
- **Uploads into a skill without a create ACL are admin-only** (#778). `/ingest/upload` and the MCP `ingest_upload` now check the
  create ACL of the skill a MIME type routes to, as they already did for an explicit `skill`.
- **`list_skills` publishes `required_when` as `[field, [values...]]`** (#777); a skill may list several values.
- **Web-workbench path overrides are named `WORKBENCH_OPT`, `WORKBENCH_NODE`, `WORKBENCH_OVERLAY_DIR`,
  `WORKBENCH_OVERLAY_SCRIPT_TIMEOUT_MS`** (#784, #785).
- **Page-bound chat stamps the author on the server** (#785); a client-supplied `author` is ignored for `page-chat:v1:` groups.

**Data rules a skill declares** (#771–#773, #775–#777, #783): field rules (`required_when`, `non_blank`, `format: date`,
`immutable`), link fields (`kind: link`, `to:` one or several skills, `acyclic`), identity uniqueness (`unique`), instance
immutability, `freeze_when`/`frozen_by_parent`, `protected: [delete, move]`, the in-use guard (`relation_in_use`, also for archive
via `update_page`), release hash binding (`pack:`), page-bound chat (`chat: {page_bound: true}`) and `instance_acl: false`.
Issues carry a stable `family` beside the code. A changeset's drafts are judged as one batch.

**Tools and transport**: `get_draft` (#777); `ingest_upload` over MCP with skill-declared retention, event kind and settlement,
XML/JSON extraction, MIME aliases and failed-extraction retry (#770, #779); a skill-declared lease fence on `create_draft` and
`discard_changeset` (#779); configurable body limits `ESCUREL_MCP_MAX_BODY_BYTES`/`ESCUREL_INGEST_MAX_BODY_BYTES`, default
16 MiB (#778); run tokens may reply into the page chat their root event names, and `append_message` takes
`page_revision_sha256` (`revision_changed`) (#785); narrowed agent tokens may carry several output skills (#768).

**Runner**: Gemini token budget carried across attempts, Vertex workload auth, a preflight error and failed plans recorded as
failures (#769). Binaries built on their own keep frontmatter key order (#786).

**VS Code**: a public, versioned API for companion extensions (client, `webviewHtml` with fonts/lang/title, theme tokens,
Overview tiles, page renderers and augments, review hooks, Inbox decorators, subject, helper credential, focus profiles)
(#774, #781, #782).

**Web workbench**: a downstream overlay contract at `/opt/escurel/overlay/` (settings, CSS, login tokens, product name, assets,
extensions, bootstrap scripts), validated at build and start (#784).

**Test support**: external issuer, write ACL, groups, fixture export and an operator bearer for the test gateway (#767, #780).

## v2026.10.21

A patch on v2026.10.19 from the heron-escurel events-to-lake window (hetzner-agent-substrate #979/#987): its Drive cold boot
died at minute 20 on one transient Drive write error and restarted into another 20 minutes. Both causes are fixed. Use this
tag for any boot over a Drive lane (heron-escurel); no migration, no stored-format change.

- **A rebuild reads the lane and writes only the index** (#763). `Indexer::rebuild` (a cold boot on a fresh data directory,
  `escurel admin rebuild`) re-derived the index through `update_page_as`, which writes the lane first, so it wrote every page
  it had just read straight back: one lane write per page (on Drive about 1.5 s each, 223 pages on heron) and one transient
  error failed the boot. The rebuild now skips the lane write; every other writer is unchanged, and `migrate-kind` still
  writes the lane because it changes the bytes. Expected boot saving on heron about 5 to 6 minutes (not measured).
- **The duckvfs lane store retries transient Drive errors** (#764). A new `RetryingStore` repeats a `temporary Drive API
  failure`, rate limit, 429/5xx, timeout or connection reset with exponential backoff and jitter; not-found, permission,
  quota and bad-key errors return at once, and a retried delete that finds the object gone counts as done. Each retry logs one
  `warn` (operation, tenant-relative path, attempt, matched marker; never the error text). New env, defaults in brackets:
  `ESCUREL_STORAGE_DUCKVFS_RETRY_ATTEMPTS` (6; `1` turns retrying off), `ESCUREL_STORAGE_DUCKVFS_RETRY_MAX_BACKOFF_MS`
  (30000), `ESCUREL_STORAGE_DUCKVFS_RETRY_TOTAL_MS` (180000).
- **A refused tail cursor resumes where the runner left off.** A gateway restarted without a shared `ESCUREL_CURSOR_KEY` signs
  cursors with a fresh random key, so the cursor a runner had stored was refused and the runner repositioned at the END of the
  label, dropping every request filed since its last read. The tail now keeps the id of the newest event it read (ledger table
  `tail_marks`) and scans the label for it; with no mark, or a purged one, it falls back to the end. The gateway also warns at
  boot when `ESCUREL_CURSOR_KEY` is unset.
- **`create_branch` refuses a name whose overlay file another branch uses** (`a/b` vs `a-b`, issue `name_collision`): one branch
  could silently overwrite the other's pages.
- **`admin purge-events` never deletes a write-back `:applied` witness**, even with `allow_protected`: the witness is what stops
  a retry writing upstream twice.
- **A same-day re-cut tag (`vYYYY.MM.DD.N`) no longer fails the VSIX job.** Cargo carried the fourth component as build
  metadata but the VSIX step did not, so `npm version` rejected it. Both now use `scripts/release-semver.sh`
  (`2026.10.15.1` -> `2026.10.15+1`, leading zeros stripped), tested in CI. `escurel-quota`'s `buckets_refill_with_time` no
  longer races its own drain (a test-only fix).
- **Docs:** `list_events` tail paging says `next_cursor`, `/metrics` is on `:9090`, the rollback section of
  `docs/deploy/kind-migration.md` names `lake-upgrade --to 9`, the Dockerfile's glibc note, and the changelog's secret naming
  (`ESCUREL_SECRET_<TENANT>__<NAME>`).

### Operators

- **No stored-format change, no migration.** Nothing here needs `migrate-kind`, `lake-upgrade` or `events-copy`.
- **What the heron window showed.** On v2026.10.19 the heron-escurel events-to-lake window (backends `ducklake`, Drive lane)
  booted from a fresh data directory for about 20 minutes and died with `lane store error: io error: duckvfs write:
  temporary Drive API failure`; the pod restarted and the second boot succeeded (ready at 14:12). With this tag the rebuild
  no longer rewrites lane pages (expected saving about 5 to 6 minutes, not measured) and lane store errors are retried, so
  one transient Drive error no longer costs a full cold boot. Keep the startup budget at 45 minutes or more.
- **Not covered.** DuckLake over `gdrive://` talks to Drive inside DuckDB, not through the lane store: a lake attach at boot
  still has no retry (the publish tick retries on its next cycle). See
  `docs/notes/discovered/2026-10-21-drive-lane-store-had-no-retry.md` and
  `docs/notes/discovered/2026-10-21-rebuild-rewrote-every-lane-page.md`.
- **Images and re-pins.** The release image is built with `REQUIRE_GDRIVE=1`: `ghcr.io/datazoode/escurel:v2026.10.21` and
  `escurel-runner:v2026.10.21`. heron-escurel moves first (watch the cold boot), then dz-escurel and both runners, one app at
  a time in their own values PRs.

## v2026.10.19

A patch on v2026.10.18 from the Drive rehearsal of the events move (hetzner-agent-substrate #982). **Use this tag, not
v2026.10.18, for `events-copy`**: the v2026.10.18 build cannot load the lab's unsigned `gdrive` extension in that job.

- **`events-copy` honours `ESCUREL_ALLOW_UNSIGNED_EXTENSIONS`.** The copy opened a bare in-memory DuckDB connection, so
  attaching a `gdrive://` lake died at `LOAD` of the unsigned mirror `gdrive` extension ("signature is either missing"),
  the same artifact the lab image bakes. It now opens with `Migrator::connection_config()` like every other lake
  connection. Found by the Drive rehearsal #982; see `docs/notes/discovered/2026-10-18-events-copy-bare-connection.md`.
- **The server image ships `events-copy-job`.** The runtime image copied only `migrate-kind-job`, so the lab Job for
  the Postgres to lake copy had no entrypoint (the v2026.10.18 image does not contain `events-copy-job`). The
  `Dockerfile` now installs `scripts/events-copy-job.sh` as `/usr/local/bin/events-copy-job`, and a `RUN test -x`
  step fails the image build if either Job script is missing or not executable; `scripts/test-image-scripts.sh`
  (run in CI) fails when a `scripts/*-job.sh` is not shipped. `docs/deploy/events-to-lake.md` lists the Job's
  environment: `ESCUREL_EVENTS_BACKEND`/`ESCUREL_CHAT_BACKEND` may stay unset during the Job and are switched to
  `ducklake` only for the server boot afterwards.
- **A cold boot now reports the index open.** The 19-21 minutes of silence in heron-escurel's cold boot (rehearsal
  #982) were not the lake attach or adopt: on a fresh data directory the boot first scans every lane page and rebuilds
  the whole index from the lane store, a step #746's progress lines did not cover. It logs
  `boot: open the index (a fresh data directory scans and rebuilds it from the lanes) is still running` every 30 s
  (target `escurel_boot`, with `elapsed_s`). See `docs/notes/discovered/2026-10-18-fresh-boot-index-open-was-silent.md`.

### Operators

- **No stored-format change, no migration.** Nothing here needs `migrate-kind` or `lake-upgrade`.
- **Measured on the Drive rehearsal (#982, a copy of heron's lake and catalog).** `events-copy` took 1 min 43 s end to end
  for 151 events, 63 drafts and 0 chat rows (dry run 9 s, apply 67 s, verification 27 s; a second apply copied nothing),
  so the default 3600 s Job deadline is ample. A cold writer boot on the Drive lake after the switch took 21 min 10 s
  (5 min 11 s for a boot with an existing local index): size the startup probe at 45 minutes or more. On real Drive
  `capture_event` takes about 6 s, `list_inbox` about 1 s and `list_events` by label 0.8-1 s (Postgres through the proxy:
  1.6 s, 0.4 s, 0.4 s), compaction did not change that. The first publish after the switch trashes about 1000 objects
  (4.8 MB) and reads can stall for about 7 minutes during that first garbage collection; the lake then stays at about
  30 files. Rollback is switching the env back: the Postgres rows are never touched.
- **Images and re-pins.** The release image is built with `REQUIRE_GDRIVE=1`: `ghcr.io/datazoode/escurel:v2026.10.19`
  and `escurel-runner:v2026.10.19`. Anything that runs `events-copy` uses this tag; the rest of the lab and heron may move
  from v2026.10.18 to it at the same time (their own values PRs, one app at a time).

## v2026.10.18

- **Drive-friendly lakes: immutable data prefix, data-driven compaction, drafts compacted, a lake-draft fix** (skill
  0.25.5). A `gdrive://` lake now declares its `DATA_PATH` `gdrive_immutable_prefixes` (DuckLake never rewrites a file
  in place), which drops the extension's ~270 ms per-open revalidation; the publish tick rewrites an append table
  (events, drafts, chat) only once it holds `ESCUREL_APPEND_COMPACT_MIN_FILES` files (default 8) instead of every tick
  (an idle system uploaded ~576 objects a day), and the drafts table, which nothing compacted, is compacted too.
  `ESCUREL_DUCKLAKE_GDRIVE_PERMANENT_DELETE=true` makes the GC delete expired Parquet instead of trashing it.
  **Fixed:** on a lake-backed drafts table the first proposal failed with `NOT NULL constraint failed:
  escurel_drafts.decided_by` (the lake table has no defaults and `create_draft` omitted `reason` / `decided_by`); a
  draft now lives its whole life on a lake. File operations per call are in `docs/deploy/lake-on-drive.md`.
- **`escurel-server events-copy`: the supported Postgres-to-lake copy of events, drafts and chat** (skill 0.25.4).
  Switching `ESCUREL_EVENTS_BACKEND` / `ESCUREL_CHAT_BACKEND` to `ducklake` alone started empty lake tables and orphaned
  the Postgres rows (inbox, pending drafts, audit events, chat). `events-copy` copies the configured tenant's rows with
  every id, `seq`, `at_ts`, `kind` and lineage column preserved; it is a dry run unless `--apply`, takes the
  single-writer lease to apply, is idempotent (keyed on `event_id` / `draft_id` / `msg_id`), never touches or deletes the
  Postgres rows, refuses to overwrite a diverged lake row, verifies row counts and checksums per table, and writes an
  `escurel:events-copy` audit event. `scripts/events-copy-job.sh` runs dry run, apply and an independent verification as
  a Job (no throwaway server). Runbook: `docs/deploy/events-to-lake.md`.
- **A writer says what it is doing while it adopts a slow lake; startup recovery is quiet on a tenant with no workflow
  runs.** The heron-escurel writer was silent for 19 minutes adopting its Drive lake (a file-by-file fetch with no
  output), which looked like a hang while a startup probe was ticking. The slow boot steps (`attach the lake`,
  `adopt the lake's content`, and the reader's adopt) now log `boot: <phase> is still running` with `elapsed_s` every
  30 s (target `escurel_boot`); the startup-budget numbers are in `docs/deploy/kind-migration.md`. And the runner's
  startup crash recovery no longer warns `workflow: gateway read failed: … unknown_skill … workflow-run` on a tenant
  that never declared the `workflow-run` skill: no skill means no runs to re-drive.
- **The SQL-source policy judges stored bindings at writer boot** (skill 0.25.3). Security pass of 2026-10-17 (F1). The
  credential/egress policy was installed on the served indexer after `Config::build` had rebuilt every `sql_view`, so a
  binding already in storage (written before the policy, or under a wider `ESCUREL_SQL_FILE_DIRS`) was re-attached with
  no check and stayed queryable for the life of the process. The policy (now parsed with the rest of the config, so an
  embedded gateway can set it from its config source) goes on the indexer before the fresh-boot rebuild and the
  lake-adopt rebuild; an out-of-policy binding's view is simply not created.
- **`/ingest` refuses the gateway's event-id namespaces; a machine cannot purge events** (skill 0.25.2). Security pass
  of 2026-10-17. `/ingest` forwarded a caller's `event_id` without the reserved-prefix check `capture_event` has, so a
  member could squat `runner-status:<runner>` or forge a `write-back:<draft>:applied` witness (a promote would skip its
  upstream write); both are `422 invalid_event_id` now, through one shared helper. `admin_purge_events` now sees its
  caller: a runner-minted admin run token (a machine) may count but not `apply` or `allow_protected`, so an agent run
  cannot erase the write-back witnesses or the purge audit row. A person's admin token is unchanged.
- **Branch merges use a per-page fork point** (skill 0.25.1). `create_branch` recorded `v0` as the fork version, so
  merging a branch that edited a page with earlier CRDT history reported a `conflict` nobody had caused, and a branch
  tombstone had no fork point at all and silently deleted a page the base had edited meanwhile. The branch's first write
  to a base page now records that page's version (`branch_page_bases`); `merge_branch` compares against it, and a
  tombstone of a page the base edited since is a `conflict`. Branches that predate the table merge as before.

- **The `duckvfs` lane store can sit on SharePoint** (skill 0.25.0). A `sharepoint://` root loads the `sharepoint`
  extension and registers its client-credentials secret from `ESCUREL_STORAGE_DUCKVFS_SHAREPOINT_*`
  (tenant, client id, client secret or `…_CLIENT_SECRET_FILE`, drive or site id, optional `GRAPH_SCOPE`). The secret
  never reaches `Debug`, logs or errors; a missing credential or target, or both drive and site id, fails config; the
  variables are refused on any other root; keys with SharePoint-forbidden characters are refused at write time. Lanes
  only: a DuckLake `DATA_PATH` on SharePoint, and baking the extension into the image, follow separately. See
  `docs/deploy/sharepoint-lanes.md`.

- **`publish-image.yml` no longer interpolates the pushed tag name into shell.** The job holds `packages: write` and
  the tag is chosen by whoever pushes it, so a name such as `v1$(cmd)` would have run in the job. The tag now reaches
  `scripts/image-tags.sh` only through the environment, is validated by `scripts/release-version.sh` before the
  buildx setup and the GHCR login, and a hostile name fails the job (`scripts/test-image-tags.sh`, run in CI).

- **The lake publish guards fail closed** (#742). `publish_snapshot` refuses an empty or partial publish by comparing the
  corpus with the lake's page count, but every count used `.unwrap_or(0)`: a count that could not be read looked like
  an empty lake and the guards did not fire (the damaged-catalog case was only stopped later, by luck, in the commit).
  An unreadable count is now `SnapshotError::LakePagesUnreadable`; a fresh lake with no `pages` table still publishes.

- **`scripts/release-tag.sh` creates the annotated tag** (#739). `git tag -a -m/-F` strips every line that starts with
  `#`, which removed the section headings and with them the breaking-change marker `release.yml` checks for in the tag
  message (the first `v2026.10.17` push failed its create-release step on exactly this). The script writes the message
  with `--cleanup=whitespace`, verifies the marker survived and never pushes (`scripts/test-release-tag.sh`, run in CI).
  The note is `docs/notes/discovered/2026-10-17-git-tag-strips-markdown-headings.md`.

### Operators

- **No stored-format change, no migration.** Nothing here needs `migrate-kind` or `lake-upgrade`. The one new table,
  `branch_page_bases`, is created on first use; branches that predate it merge as before.
- **Moving events, drafts and chat to the lake** (a deployment that ran the Postgres backend): do not just switch
  `ESCUREL_EVENTS_BACKEND` / `ESCUREL_CHAT_BACKEND`; run `events-copy` first with the writer stopped
  (`docs/deploy/events-to-lake.md`: dry run, apply, second dry run, switch the env, boot, verify; rollback is switching the
  env back, the Postgres rows are never touched). Size the Job deadline from a rehearsal on a copy of the real catalog and
  lake; there is no measured `events-copy` timing yet (the Drive rehearsal is tracked as hetzner-agent-substrate #982).
  For a `gdrive://` lake read `docs/deploy/lake-on-drive.md` (immutable data prefix, `ESCUREL_APPEND_COMPACT_MIN_FILES`,
  `ESCUREL_DUCKLAKE_GDRIVE_PERMANENT_DELETE`). The lake-draft fix in this release is required before any draft is created
  on a lake backend.
- **Startup budget on a Drive lake.** A cold writer boot on the real heron Drive lake took 19 minutes against 5.5 minutes
  in the rehearsal; keep the startup probe budget above that (`docs/deploy/kind-migration.md`), and expect the new
  `boot: <phase> is still running` lines while it adopts.
- **Behaviour changes to know about.** `/ingest` answers `422 invalid_event_id` for ids that start `runner-status:` or
  `write-back:`; a runner-minted admin run token can no longer `apply` or `allow_protected` an `admin_purge_events`
  (a person's admin token is unchanged); a stored SQL-view binding that the current `ESCUREL_SQL_*` policy forbids is
  no longer re-attached at writer boot.
- **Images.** The release image is built from the tag with `REQUIRE_GDRIVE=1` (a missing gdrive v1.5.6 artifact on the
  erpl.io mirror fails the build): `ghcr.io/datazoode/escurel:v2026.10.18` and `escurel-runner:v2026.10.18`. Untagged
  `main` images are built without that guarantee and are for rehearsals only.
- **Consumer re-pins.** At the time of writing the hetzner lab runs `dz-escurel` and both runners on `07ca8c19cb97` and
  `heron-escurel` on the v2026.10.17 build (`069f63f22c2a`); all of them move to the `v2026.10.18` tag (or its commit) in
  the lab's own values PRs, one app at a time. heron (agent-template pin), anofox-evolve (Cargo `rev`) and
  datazoo-ai-engineering follow the checkout.

## v2026.10.17 — BREAKING (stored format, wire, skills, agent behaviour)

**Read first:** [`docs/deploy/kind-migration.md`](docs/deploy/kind-migration.md) (stop-first upgrade, backup,
rollback) and the consumer checklist in
[`.claude/skills/escurel-platform/CHANGELOG.md`](.claude/skills/escurel-platform/CHANGELOG.md) (0.7.0 – 0.24.0).
Skill version `0.24.0`. Every consumer that writes pages or reads the tool surface moves in the same window.

### Fixed

- **The publish guards fail closed when the lake cannot be read.** The empty-corpus and the partial-corpus guards
  compared against a lake page count that was `.unwrap_or(0)`: a failed read looked like an empty lake, so neither
  guard fired and the whole-corpus overwrite went ahead. The count now propagates its error
  (`SnapshotError::LakePagesUnreadable`, refused before any table is overwritten); a lake with no `pages` table yet
  is still empty and publishes.

- **Object-store directory connectors can be operator-authorized.** `json_dir` and `parquet_dir`
  sources under `s3://` / `gs://` now pass the gateway policy when their URL lies beneath an
  `ESCUREL_SQL_OBJECT_PREFIXES` entry. The default remains deny. Consumers with remote directory
  sources must set this variable before upgrading (`s3://bucket/demo/` for that directory); DuckDB
  still needs object-store credentials and endpoint access.

- **List cursors are bound to their tenant and list; `ESCUREL_CURSOR_KEY` needs 32 bytes** (#684): a cursor issued
  for one skill/list no longer opens on another (it used to return a silently empty page); a shorter key stops
  the server at boot.
- **One `quote_ident` helper for every column spliced into rows SQL** (#685): a column named `we"ird` made
  `expand` a syntax error and `search` find nothing; a hostile name could close the identifier.
- **A run-bound token cannot file `escurel:` events, system events or workflow steps** (#686): the runner mints
  ADMIN run tokens, and the guard was `!is_admin`. The runner's own admin identity is unaffected.
- **A branch is its author's workspace** (#687, tightened in #714): nobody else writes into it, merges it or
  abandons it; only its author (for a machine's branch: the person who minted the token) or an admin decides
  it. Anyone else gets `unknown_branch`, the same answer as a missing name.
- **A hand-written `scenario:` naming a registered branch follows branch ownership** (#708): `update_page`
  without a `branch:` argument used to drop the page into another author's branch, and that author's merge
  would land it. Now author/admin only, others get `unknown_branch`; an unregistered name stays the legacy overlay.
- **Write-back audit rows are per attempt** (#705): REST write-back records
  `write-back:<draft>:<n>:applying|failed`, so a stale record from an earlier outage can no longer license a
  second send of a non-idempotent write that may already have applied (at-most-once holds).
- **Release machinery** (#704, #713): the tag build fetches dependencies before refreshing the lockfile (the
  first tag build would have failed on a cold runner); lightweight tags are refused; a same-day re-cut
  `vYYYY.MM.DD.N` builds as `YYYY.M.D+N`; the release's first job gates the tag on a green main push CI run of
  that exact commit (an ancestor of `main`, waits up to 30 min; `RELEASE_SKIP_CI_GATE=1` is the logged escape
  hatch). The image bakes `anofox_optimize` for DuckDB 1.5.6 (#706).
- **Lab-move engine fixes (#716, #718, #719, #720).** The lake (DuckLake) now has a supported upgrade path and a
  cutover that cannot leave it stale or strand the previous release:
  `escurel-server lake-upgrade [--dry-run]` re-pins a manifest one declared-compatible schema version behind
  (9 to 10), and a writer also adopts such a lake at boot; `escurel-server lake-upgrade --to N` walks the pin
  back to a declared-compatible older version (idempotent; both need the writer stopped). `migrate-kind
  --apply` on a DuckLake writer publishes the migrated corpus (`lake_published` in the report), and a writer
  that adopts a lake still holding `type:` pages boots QUARANTINED instead of serving it. `publish_snapshot`
  refuses a partial index and a corpus under half the lake's pages (optional `allow_shrink` for a deliberate
  mass delete); `rebuild` keeps running when its client gives up (`escurel admin rebuild --timeout-secs`).
  `migrate-kind-job` creates its data dir and DuckDB home itself (fresh volume), drops the Deployment's
  `ESCUREL_AUTH_*` / webhook / periodic-publish / metrics environment for its loopback-only throwaway server (an
  OIDC-enabled gateway migrates with the same pod environment), and takes the one-way lake re-pin LAST, so a job
  that fails on config, auth or a conflict leaves the lake where the previous release can still boot it.
  See `docs/deploy/kind-migration.md` and `docs/notes/discovered/2026-10-15-lab-cutover-lessons.md`.
- Links that escape the exposed directories fail closed when the directory cannot be read (#681); the
  `rows_get` timeout test is deterministic (#683).
- **`capture_event` no longer tells a guesser that an event exists (security, #721).** Under
  `ESCUREL_EVENT_ACL=enforce`, re-capturing a stored `event_id` the caller may not read used to answer
  `replayed: true`, which revealed the id. A hidden collision now reads exactly like a first capture; the flag
  stays for the caller who may see the stored event (and in `log` mode).
- **A `**` glob over a tree deeper than the symlink check can inspect is refused (security, #723).**
  `json_dir` / `parquet_dir` globs containing `**` were checked only 8 directory levels deep, so a symlink out
  of the allowed directories below that passed unchecked. A directory below the cap now counts as escaping:
  a legitimate `**` glob over a tree deeper than 8 levels is refused; use a more specific glob or keep the
  tree shallower.
- **A tag name can no longer inject into the release job (security, #724).** The version taken from the tag
  now passes `scripts/release-version.sh` (digits and dots, optional `-prerelease` and `+build`) before it reaches
  a shell step; a tag such as `v1$(cmd)` is refused. Exploiting it needed push access.

- **Write-back: at-most-once unless the skill declares idempotency; `base_etag` is required (security / data
  integrity).** Every REST write used to count as repeatable, so an upstream that ignored `Idempotency-Key` could
  apply a change up to three times after a lost answer; an optional `base_etag` meant a proposal without one
  silently overwrote an upstream edit. A REST write is now retried only with `write: {…, idempotent: true}` (or
  `PUT`); a proposal without `base_etag` is refused `write_back_base_etag_required`.
- **Reading one row had no statement timeout (availability).** `expand` of a row page (`rows_get`) was the only
  rows read outside the watchdog, so a slow source held the tenant's single DuckDB connection (and every search
  and write behind it) as long as it liked. It now runs under the same `ESCUREL_ROWS_QUERY_TIMEOUT_SECS` interrupt
  as listing and search.
- **An unlistable directory failed the symlink check open (security).** `links_escape` (`json_dir` / `parquet_dir`
  sources, secret files) treated a directory it could not read as "nothing escapes"; it now fails closed.
- **A tenant could name another tenant's secret (security).** `ESCUREL_SECRET_<TENANT>__*` was matched by
  prefix over a lossy tenant encoding: tenant `a` matched `ESCUREL_SECRET_A__B__X` of tenant `a__b`, and
  `a-b` / `a_b` shared one namespace. The secret name may no longer contain `__`, an id whose token holds
  `__` has no env namespace, and `tenant_create` refuses a second tenant with the same token. Skill `0.19.2`.
- **A machine could land a skill page through a branch (security).** `merge_branch` judged each page of a
  machine's branch by an INSTANCE-shaped probe id, so a branch edit of an `autonomy: auto` skill's page, or a
  brand-new skill page, merged unreviewed (the skill page carries the gate's own configuration). The probe is
  now the page the merge would actually write; a machine's merge touching a skill page or a review skill is
  refused `review_required`, a person's merge is the review.

- **A refused read is an error, never an empty success (security-relevant).** `escurel-client` decoded
  the `structuredContent` of a refused read (`isError: true`, `{ok: false, issues}`) into a response type
  whose fields all default, so an ACL denial, `invalid_limit`, `query_not_found`, … returned `Ok(<empty>)`:
  a silent partial read. Now `Error::Refused(Refusal{issues, payload})` (shared reader
  `escurel_types::call_result`). The write family and `validate` still return the typed answer with
  `ok: false`; `rebase_pack` stays a report. Same fix in the echo and Gemini harness clients, the CLI
  (non-zero exit), the test-support client (`call_ok`), the VS Code client and the Dart client. A guard
  test fails if a crate reads a result's `structuredContent` outside the shared reader. Skill `0.15.1`.

### BREAKING

- **`ESCUREL_WRITE_ACL` defaults to `enforce`** (was `off`; an unrecognised value also enforces). Writes by a
  caller who is neither the instance's owner nor an admin are refused with `forbidden`. Set
  `ESCUREL_WRITE_ACL=off` to keep the old behaviour, or `log` to find the callers first. Skill `0.17.1`.

- **The page kind is `kind:` (was `type:`).** `type: skill|instance` is removed — a hard cut with no
  compatibility switch. A tenant whose lane still holds such pages boots **QUARANTINED** (up, answers only
  `migrate_kind` / `compact_lanes`); writes with the old key are refused (`frontmatter_type_removed`).
  Migrate with `escurel admin migrate-kind --tenant <t> [--apply]` (dry run by default, idempotent; rewrites
  pages, open drafts and CRDT snapshots; skips signed pack pages — the publisher re-exports). The `issue`
  skill's `kind` data field is now `issue_kind`.
- **Wire: `page_type` is `page_kind`** (`search` argument, `PageRef` answers, CLI `--page-kind`, Rust
  `PageKind`, Dart `PageKind`). A caller still sending `page_type` is refused, not silently unfiltered. The
  derived SQL column `pages.page_type` keeps its name.
- **Workflow-run pages: `status` → `run_status`** (migrated by `migrate_kind`; a tenant's own `status` data and
  the DB/API `status` fields are untouched).
- **A skill's `actions:` is a list of objects** (`{name, kind: event|prompt, label, event?, prompt?}`, Peacock's
  form); bare skill-id strings are rejected (`action_invalid`). `list_skills` returns the objects.
- **`resume_cursor` is removed — `next_cursor` is the only cursor name** (`list_inbox`, `list_events`, and the
  `list_*` tools). `next_cursor` is now where the page ENDED (present iff the page is non-empty; null only when
  there is nothing more); the new `has_more: true` says rows already lie past the page, so a client that pages
  "until `next_cursor` is absent" still terminates after one extra empty call. `list_drafts` / `list_changesets` /
  `list_branches` now take `limit` + `cursor` (the limit applies AFTER the caller's visibility filter).
- **Cursors are opaque** (`r1.` / `u1.` envelopes for rows and REST/MCP rows): a cursor from before the release
  answers `invalid_cursor` ("restart without `cursor`"). A `limit` outside the tool's declared range is refused
  with `invalid_limit`.
- **MCP `tools/call`: `content[0].text` is a short summary**; `structuredContent` is the full payload. A client
  that parsed the text block as JSON must read `structuredContent`; a client that can read ONLY the text block
  (some chat hosts) now sees the summary, not the data.
- **Read tools answer domain mistakes as `isError: true` + `issues[]`** (the shape write tools always had):
  `invalid_cursor`, `field_not_filterable`, `query_not_found`, `query_not_runnable`, `invalid_query_params`,
  `endpoint_not_registered`, `use_write_back`. JSON-RPC errors remain for malformed requests.
- **`autonomy: review | confirm` is ENFORCED at the gateway for MACHINE callers** (tokens carrying `run_id` /
  `skill` / `act.sub` claims): `update_page` and the `close_session` write-through answer
  `{ok: true, held_for_review: true, draft}` and nothing lands until a reviewer promotes; `move_page` /
  `delete_page` answer `review_required`. People on plain agent-role tokens, admins and `autonomy: auto` skills
  are unchanged, and promoting always lands. An agent flow that wrote review-skill pages directly must now propose
  drafts.
- **Write-back drafts can only be promoted by a non-agent token** (`promote_requires_human`); a run token can
  propose a write-back but never approve it.
- **Credentials a tenant may reference are allow-listed** (`secret_ref`): `gsm:` / `env:ESCUREL_SECRET_*`, extra
  `env:` names only via `ESCUREL_SECRET_ENV_ALLOW`, `file:` only under `ESCUREL_SECRET_FILE_DIRS`
  (default `/run/secrets`). An existing endpoint registered with an arbitrary `env:`/`file:` reference stops
  resolving until the operator allows it.
- **`trust` on projections is `external` (REST/MCP) or `source` (SQL rows)**; treat both as data, never as
  instructions.
- Details line by line: [`docs/notes/breaking-wire-changes.md`](docs/notes/breaking-wire-changes.md).

### Added

- **A stopped runner's status record is collected.** Every `report_runner_status` also deletes the other
  `runner-status:*` records of the tenant whose runner has been silent for longer than `max(10 x its own
  status_interval_s, 1 h)` (judged by the reporting runner's report time), so the record a replaced pod leaves
  behind (the default `ESCUREL_RUNNER_ID` is the pod name) goes away on its own; legacy heartbeat rows and live
  runners are never touched. The candidate read is one primary-key range on the attached-Postgres table. Skill 0.24.0.
- **`report_runner_status`: a runner's health is ONE overwritten record, not a heartbeat stream.** The runner
  overwrites `runner-status:<runner_id>` (label `escurel:runner-status`, `kind: system`; `state` is the title and
  the body gains `state`, `status_interval_s`, `started_at`, `last_change_at`) through the new MCP tool, so the
  events table stops growing (the lab's reached ~58k rows). Native `INSERT … ON CONFLICT DO UPDATE` on the
  attached-Postgres table, UPDATE-then-INSERT on the local and lake tables. A runner's first report deletes its
  older heartbeat rows (58k rows: ~130 ms against a local Postgres). Runner admin identity only; no webhook or
  `/ws` event; `capture_event` refuses ids starting `runner-status:`. A runner whose gateway answers `-32601`
  (an image older than this) falls back to the old heartbeat events; the VS Code Runs view reads the interval
  from the record (`status_interval_s`), else from that runner's own rows. 88 tools; skill 0.23.0.
- **`admin_purge_events` / `escurel admin purge-events` / `AdminClient::purge_events` (#731).** An audited
  retention tool for one event label: a dry run unless `apply`, refused without a bound (`before_ts` and/or
  `keep_latest`), protected audit labels need `allow_protected`, and an applied purge leaves an
  `escurel:event-purge` event (87 tools with it; 88 with `report_runner_status`). Use it to clear a heartbeat backlog on a lake that never ran the
  keep-50 prune.
- **`migrate-kind --rename-field OLD=NEW` (#728, job env `ESCUREL_MIGRATE_KIND_RENAME`)** renames a domain
  field that collides with the page-kind key (a `kind:` data field) while the pages migrate; see
  [`docs/deploy/kind-migration.md`](docs/deploy/kind-migration.md).

- **A skill can declare an Overview tile (#700).** `overview: {tile, status_field, delayed_when, sort_field?}` in a
  skill's frontmatter names a status field and the values that count as delayed; the VS Code Overview board shows
  one tile per declaring skill ("N delayed", the top three by `sort_field`, each opening the instance). It is
  lint-only: a malformed declaration yields a warning (`overview_invalid`, `overview_tile_invalid`,
  `overview_status_field_unknown`, `overview_delayed_when_invalid`), never a refusal. `list_skills` rows carry the
  declaration as `overview` (additive; omitted when the skill declares none or the declaration is invalid). Counting is bounded
  (5 pages x 100): when the budget is spent the tile reads "N+ delayed"; a rows skill whose status field is not
  `filterable` gets no tile. No new tool. Skill `0.20.0`.
- **The runner publishes its harness allow-list (#701; BACKEND_GAPS PR-10).** The `escurel:runner-status` body
  carries `harness_allow` (sorted names only, from `ESCUREL_RUNNER_HARNESS_ALLOW`) next to the default `harness`.
  The extension's Start offers a picker when `escurel.harness` is unset and the list has more than one entry, and
  never sends a harness outside it: a configured name that is not on the list shows a warning (Start with
  default / Open settings / Cancel) instead of silently running another engine. An older runner (field absent)
  behaves as before. Additive. Skill `0.20.0`.
- **Close thread (#702; BACKEND_GAPS PR-9).** A thread can be closed and reopened with a durable event:
  `capture_event` label `escurel:thread-closed`, body `{action: close|reopen, root_event_id, reason?}`. Whoever
  may read the root event may close or reopen (closing is shared and visible: the Closed group shows who and
  when); run-bound and other machine tokens are refused, an unknown or unreadable root answers `event_not_found`
  (no existence oracle), re-asking the current state is a no-op answer (`already: true`, nothing filed or
  broadcast). Stored `kind: system` with a server-stamped `provenance.thread` (a caller's own `provenance` is
  dropped; `reason` is capped at 200 characters, the stamped `root_title` is cut to 200). Read closed threads with
  `list_events {label_skill: "escurel:thread-closed"}`. Additive: one label, one response flag. The Threads outline
  ends in a collapsed Closed group with Close / Reopen commands. Skill `0.20.0`.

- **Migration tooling (#654).** `escurel admin migrate-kind` runs the apply in a spawned server task (a client
  that disconnects or is killed no longer cancels it; `--timeout-secs` is opt-in, progress is printed every
  30 s); a durable marker `meta/migrate-kind.pending` keeps a tenant QUARANTINED across a `kill -9` mid-apply
  and the re-apply is idempotent; `escurel admin migrate-kind-files --apply <dir>` rewrites a checkout
  offline (temp files are created `O_EXCL`, a planted symlink is never followed); BOM / CRLF pages parse and
  migrate; `scripts/migrate-kind-job.sh` is the stop-first one-shot job (non-zero on a conflict or a boot
  failure). Measured: 20k pages ≈ 11 min, 406 MB peak.
- **Operator safety (#654).** `config_keys` registers every `ESCUREL_*` variable and generates
  `docs/deploy/env.md` (a test fails on an undocumented or phantom key); the structured log goes through a
  bounded non-blocking writer (a stalled reader can no longer freeze the gateway; dropped lines are counted
  in `escurel_log_lines_dropped_total`, lines are capped at 16 KB); a non-loopback listener with no OIDC
  issuer logs a loud warning and a `/readyz` notice; Postgres attaches carry `connect_timeout`
  (`ESCUREL_SQL_CONNECT_TIMEOUT_SECS`) and the rows query bound is `ESCUREL_ROWS_QUERY_TIMEOUT_SECS`;
  orphan `*.md.tmp` files are swept at boot; CI runs the live-postgres suites nightly, the full ten-point
  SIGKILL test, a PR-time Docker image build, and tests that used to skip silently are `#[ignore]`d.
- **Typed-client refusals (#654).** `escurel-client`, the VS Code client and the Dart client treat
  `isError: true` as an error (`Error::Refused`), never as an empty result; `held_for_review` and `draft`
  are typed on `update_page`.
- **Anofox Evolve (#653, #657, #658).** Owner-scoped Evolve controls: `evolve_validate` and the new
  `evolve_compare` control labels target an owner-private `evolve_comparison` page, are stamped and attested
  by the gateway, never dispatched by the runner; prepared training sources and private holdout terms are
  reviewed from the Workbench. The VS Code extension gains a **Scenarios** view (seed-vs-winner diffs over the
  signed-in token, a page is trusted only if it carries the record's result hash), "New scenario comparison"
  and the Compute comparison action, plus Evolve plan review in the native window.
- **Evolve data rules (#717).** `capture_event` accepts three more owner-gated Evolve control labels,
  `evolve_rules_propose`, `evolve_rules_test` and `evolve_rules_approve`. They need `mode: run`, approval needs an
  explicit boolean `confirm` plus the exact test hash the page carries, and they are private to the author and
  never run by the runner. The Evolve event labels now come from one shared table in `escurel-types`. No wire or
  lake change.
- **DuckDB 1.5.6 (#659).** `duckdb` / `libduckdb-sys 1.10506.0`; the image's `DUCKDB_VERSION=v1.5.6`.
  DuckDB extensions are version-locked, so `anofox_optimize` and `gdrive` were rebuilt for 1.5.6 (the erpl
  mirror now serves both). The `gdrive` bake is conditional: when the mirror has no artifact for the pinned
  DuckDB the image is built WITHOUT gdrive with a loud warning (`REQUIRE_GDRIVE=1` fails the build instead);
  the `anofox_inventory` extension test skips with a clear message on a DuckDB-version mismatch.
- **VS Code workbench (#660).** **Focus mode** (`Switch to focus view` / `Leave focus view`: a calm window
  without menu bar, command center, status bar, breadcrumbs or minimap; the person's own settings are
  remembered and restored exactly) and the **Overview board** ("Today": decisions waiting, agent activity,
  needs attention, open items per skill, recently finished) as the first screen; the light **Escurel Calm**
  theme with WCAG contrast guards (thread connectors have arrowheads and ≥ 3:1 contrast, outlined state chips,
  button borders); editor tabs name the page or skill; the run trace has a time axis and each step opens to
  what it asked and got back; records whose skill names a report show that report's KPI figures and tables;
  "Preview with parameters" on query pages; "Explain this view" on every view; keybindings `ctrl+alt+r/a/i/k`.
- **Source-to-Deliver demo (#660).** `editors/vscode/demo/s2d/`: the three S2D stories (supplier exception,
  transport consolidation, aftermarket last-time-buy) synced from the shared seed, rehearsal script, held
  proposals in "Awaiting you", optimizer pages when the `anofox_optimize` extension matches the gateway's
  DuckDB. Anonymised, illustrative data.
- **Web workbench (#660).** `deploy/web-workbench/`: a code-server image with the VSIX preinstalled, the
  calm layout baked in, an Escurel-skinned login page generated from the Calm theme tokens, a password gate
  (12 characters by default, `MIN_PASSWORD_LENGTH` may lower it to 8, placeholders refused), no terminal
  (`node-pty` removed), no marketplace, non-root, read-only root filesystem; `compose.yaml` +
  `compose.s2d.yaml` give the full S2D demo in one command, `compose.proxy.yaml` a Caddy TLS front;
  `s2d-up.sh` / `s2d-reset.sh`; `scripts/web-workbench-smoke.sh`. See `docs/deploy/web-workbench.md`.
- `folder:`, `role:`, `tags:` and the OKF vocabulary (`title`, `resource`, `generated`, `verified`, `status`,
  `stale_after`, `sources`) on skill pages; the Knowledge tree in the VS Code extension shows them.
- `backend.instances: rows` (one instance per row of a `sql_view`, optional linked markdown), REST (`openapi`)
  and MCP rows, `describe_backend`, and **human-gated write-back** (draft + promote, etag conflict check,
  idempotency key, bounded retries, dead-letter, audit).
- **SQL rows over real databases and write-back to them.** `connector: postgres | mysql | sqlite` with
  `instances: rows`; credentials as `secret_ref` references (inline `secret` deprecated), checked against the
  egress policy (`ESCUREL_SQL_FILE_DIRS` for SQLite files); a row of a skill with `writable_columns` changes
  through the same draft → human promote → guarded single-transaction `UPDATE` flow as REST/MCP rows. Postgres
  attaches enforce the statement timeout server-side. The image bakes the `sqlite` DuckDB extension next to
  `postgres`. Verified against a real Postgres container and a real SQLite file. Conflict detection is
  etag-only (the row's values at read time); a `version_column:` option was considered and dropped (2026-10-09).
  **MySQL / MariaDB are refused (skill `0.19.3`)**: it has no integration test, no CI job, no timeouts and was never
  DNS-pinned like Postgres, so `register_credential` answers `connector_not_supported`, a skill page with
  `backend.source.connector: mysql` fails `validate` and is not stored, and the image does not bake
  `mysql_scanner`.
- **Run traces keep what each tool call asked and got back.** `get_run_tool_calls` rows gain optional
  `args_summary` / `result_summary` (2 KB, credentials redacted by key and by pattern, content bodies reduced
  to sizes); the VS Code run detail shows them per step. `ESCUREL_TOOLCALL_DETAIL=off` records sizes only.

### Operators

- **Events on an attached Postgres catalog run inside Postgres (#729).** `list_inbox`, paged `list_events`,
  `capture_event` and both prunes no longer read the whole `escurel_events` table through the scanner
  (5,003 rows read for `list_inbox(5)` over 5,000 hidden heartbeats, now 2); attach creates the supporting
  indexes. Pruning a 59k-row backlog on a clone took 1.1 s. See
  `docs/notes/discovered/2026-10-16-events-table-read-in-full-on-attached-postgres.md`.
- **`ESCUREL_RUNNER_STATUS_INTERVAL` defaults to 5 minutes (was 30 s) (#730).** The real lake-churn amplifier is
  heartbeat rate x `ESCUREL_SNAPSHOT_KEEP` (2100 snapshots cover ~11 h at 30 s); compaction + GC already bound
  the catalog. Set the variable explicitly if you depend on the old cadence.
- **Sizing the migration Job (measured on a Drive-backed lake, 226 pages).** A rehearsal of `migrate-kind-job` on a
  copy of heron-escurel's lake (Drive, ~1.6 s per operation) took 67 min: boot 5.5 min, dry run 5 min, apply plus
  lake publish 46 min, second boot 5.5 min, verify 5 min, `lake-upgrade` 6 s. Size `ESCUREL_JOB_DEADLINE_SECS` for
  at least 10800 s for a lake of that kind (the default 3600 is for local/object-store lakes), keep the Drive token
  fresh for the whole run, and give the Job the same environment shape as the Deployment (Cloud SQL proxy sidecar,
  the workload-identity credential file via `GOOGLE_APPLICATION_CREDENTIALS`, the gdrive v1.5.6 extension paths,
  a writable `ESCUREL_SERVER_DATA_DIR`, `ESCUREL_CURSOR_KEY`, the Gemini key). A domain field called `kind` is
  carried across with `ESCUREL_MIGRATE_KIND_RENAME=kind=system_kind`.
- **`/readyz` reports quarantine** without failing: still 200 (so the migration can run) with
  `x-escurel-quarantined: 1` and a JSON `notices` body; new metrics `escurel_tenant_quarantined{tenant}`,
  `escurel_migration_pending`, `escurel_semantic_search_enabled`, `escurel_egress_total{outcome}`,
  `escurel_write_back_total{outcome}`, `escurel_source_unavailable_total{kind}`. **Do not gate traffic on the
  status code alone**; deploy stop-first with the migration before the swap.
- **New env (every key is in the generated table [`docs/deploy/env.md`](docs/deploy/env.md), the source of truth; `docs/deploy/README.md` explains the groups):** `ESCUREL_EGRESS_ALLOW_LOOPBACK` (dev/tests only —
  never in production), `ESCUREL_EGRESS_MAX_RESPONSE_BYTES`, `…_TIMEOUT_MS`, `…_MAX_CONCURRENCY`,
  `…_RATE_PER_SEC`, `…_WRITE_RETRY_BACKOFF_MS`, `ESCUREL_SECRET_<TENANT>__<NAME>` (secrets are per tenant; the earlier global `ESCUREL_SECRET_<NAME>` is gone), `ESCUREL_SECRET_ENV_ALLOW`,
  `ESCUREL_SECRET_FILE_DIRS`, `ESCUREL_SHUTDOWN_DRAIN_SECS` (graceful-stop deadline, default 25). An unparsable `ESCUREL_EGRESS_*` value now **fails the boot** (it used to be
  silently ignored).
- The server image runs **non-root (uid 65532)**, ships the `escurel` CLI (`docker exec … escurel admin …`), and
  fetches the gdrive DuckDB extension over https. `escurel-server --help` / `--version` no longer boot the
  server.
- `tenant export` is refused while a tenant is quarantined: take the pre-upgrade backup with the server stopped
  (`tar` the tenant directory), see the runbook.

## v2026.07.13

### Changed

- **BREAKING: removed the gRPC transport.** HTTP (MCP-over-HTTP +
  WebSocket) is now the only transport. Deleted the `escurel-proto`
  crate and the `:8081` gRPC listener. `escurel-client` now speaks
  MCP-over-HTTP, and the `escurel` CLI / `escurel-tui` default
  `--server` to `http://127.0.0.1:8080`. Admin/operator capabilities
  are now admin-role-gated MCP tools on `POST /mcp` rather than a
  separate gRPC service. Long-running admin ops (`rebuild`,
  `compact_lanes`, `tenant_export`, `tenant_import`) are blocking
  JSON-RPC calls that return their final result directly; tarballs are
  carried base64-encoded in the JSON (`tenant_export` →
  `{tarball_b64, bytes}`, `tenant_import` takes
  `{tenant_id, tarball_b64}`) rather than as gRPC streams.
  `live_session` runs over the WebSocket at `/ws`.

### Client

- `escurel-client` admin + streaming surface: an `AdminClient` for the
  unary `EscurelAdmin` RPCs (tenant CRUD, audit, quota, health,
  `attach_external`, `embedding_reload`, `compact_lanes`) plus the
  server-streaming (export / rebuild / compact) and client-streaming
  (import) flows, and the agent event/validate RPCs (`capture_event` /
  `list_inbox` / `list_events` / `assign_event`, `validate`).

### CLI

- Rebuilt the `escurel` CLI as a gh/aws-style noun-verb tree over
  `escurel-client` (`skill`, `instance`, `page`, `link`, `event`,
  `query`, `chat`, `admin`, plus top-level `search` / `resolve`), with
  a global `--format json|table` flag and a JSON-on-stderr error
  contract (non-zero exit) for agent consumption.
- New `escurel ui` subcommand launches the interactive terminal
  browser against the same `--server` / `--token`.

### TUI

- New `escurel-tui` crate: a k9s-style interactive terminal UI
  (ratatui + crossterm) over `escurel-client`. Elm-style `App`
  (navigation stack skills → instances → entity, inbox + per-instance
  event history, outgoing links + backlinks, `/` filter, `?` help)
  with a panic-safe terminal guard and a real crossterm event loop.
  Logic is terminal-free and exercised against a real gateway via a
  ratatui `TestBackend` (no mocks); run it with
  `scripts/verify-tui.sh`.

## [1.0.0] — 2026-05-26

First stable release. The v1 cut-line in
[`docs/spec/roadmap.md`](docs/spec/roadmap.md) is met.

### Agent surface (14 tools, on MCP-over-HTTP)

- Read: `search` (hybrid vector + FTS, RRF-fused), `resolve`,
  `expand`, `neighbours`, `list_skills`, `list_instances`,
  `run_stored_query`, `validate`.
- Write: `update_page`.
- Live CRDT: `open_session` / `apply_op` / `close_session` over
  HTTP and the WebSocket `/ws`
  attach path (Loro engine, per-page `LiveDoc` actor, op-log +
  snapshot persistence, two-stage external-edit reconciler).
- Chat history: `append_message` / `list_messages` (per-chat-group
  conversation log).

### Admin surface (admin-role-gated MCP tools)

Tenant CRUD, streaming export/import, audit, streaming rebuild,
`attach_external` (read-only external catalog), `embedding_reload`
(degraded-start recovery), `compact_lanes` (subsumed op compaction),
`quota_get`, and an auth-free `health`.

### Storage & retrieval

- DuckDB-only per-tenant store (vss + fts extensions); HNSW dense
  vectors + FTS, fused with Reciprocal Rank Fusion.
- LaneStore trait with **S3 (Hetzner Object Storage) as the
  production default** and a local-FS dev backend.
- Crash-recovery: mid-write transaction rollback; cattle-node-loss →
  automatic rebuild-from-markdown on boot.

### Embeddings

EmbeddingGemma in candle (CPU default) behind a reloadable seam;
Gemini as an optional hosted provider; a zero/hash embedder for the
dev loop.

### Transports, auth, quotas

axum HTTP gateway (MCP/JSON-RPC framing + `/ws`),
OIDC JWT verification with JWKS caching, token-bucket quotas across
three dimensions (queries, writes+embeds, concurrent sessions).

### Operability

- 12-factor `escurel-server` binary: `ESCUREL_*` config (over TOML),
  ports 8080/8081, graceful SIGTERM, degraded-start.
- OpenTelemetry traces + Prometheus `/metrics` + structured JSON
  logs with `request_id`.
- Substrate deployment artefacts: Nomad jobspec set, Packer
  golden-image fragment, tenant-export shipper periodic job,
  Tailscale ACL fragment, and a three-target deploy guide.
- `cargo deny` license + advisory audit; `Cargo.lock` committed.

### Developer experience

- `escurel-client` typed RPC wrapper (leaf crate — no server deps).
- `escurel-test-support`: `EscurelProcess` + `AuthMode::TestIssuer`
  + `FixtureBuilder` + `McpTestClient` — spawn escurel in a
  downstream app's tests without re-deriving the JWKS/RSA harness.
- `escurel` CLI; `examples/echo-app` demonstrating the chaining
  recipe.

### Engineering process

Built red→green TDD with no-mock integration tests as the merge
gate. GitHub Actions CI (paused during bootstrap) is **re-enabled**
at this release for every push to main and every PR.
