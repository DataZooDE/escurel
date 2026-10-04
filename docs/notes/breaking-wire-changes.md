# Breaking wire changes (this release)

One line per change. Folded into the root `CHANGELOG.md` BREAKING entry and `docs/deploy/README.md`.

- `list_inbox` / `list_events`: `resume_cursor` is REMOVED. `next_cursor` is now where the page ENDED
  (present iff the page is non-empty; null only when there is nothing more to read), and a new
  `has_more: true` says rows already lie past the page. A client that paged "until `next_cursor` is
  absent" still terminates (one extra empty call); use `has_more` to skip it. A tail polls from
  `next_cursor`. Consumers updated in-repo: escurel-types/client, escurel-runner, the VS Code
  extension (`src/client/types.ts`, run/thread/runner loops), the Dart explorer kit.
- `list_instances` (rows skills), REST/MCP row cursors: the cursor is now an opaque versioned envelope
  (`r1.` / `u1.` + base64url). A cursor from before the release answers `invalid_cursor`
  ("cursor invalid or expired; restart without `cursor`").
- `limit` outside the range a tool's schema declares is now refused with `invalid_limit` (it was
  accepted silently or failed with a Rust type name).
- Read tools answer DOMAIN mistakes as `isError: true` + `issues[{code, location, message, suggestion?}]`
  (the shape write tools always had) instead of a bare JSON-RPC error: `invalid_cursor`,
  `field_not_filterable`, `query_not_found`, `query_not_runnable`, `invalid_query_params`,
  `endpoint_not_registered`, `use_write_back`. JSON-RPC errors remain for malformed requests.
- `list_drafts` / `list_changesets` / `list_branches` accept `limit` + `cursor` and return
  `next_cursor`; `limit` now applies AFTER the caller's visibility filter.
- `tools/call` results: `content[0].text` is now a SHORT SUMMARY (what came back, counts, "Full result in
  structuredContent."; a refusal's summary carries the first issue's code and message), no longer the
  payload as JSON. `structuredContent` is unchanged and is the full result. A client that parsed
  `content[0].text` as JSON must read `structuredContent` (every in-repo client already did; the Rust
  client, the extension and the Dart client keep a fallback that parses the text only when a legacy
  gateway sent no `structuredContent`). A client that can read ONLY the text block (some chat hosts)
  now sees the summary, not the data.
- `autonomy: review | confirm` (and any unrecognised value) is ENFORCED at the gateway for MACHINE callers
  (tokens with `run_id` / `skill` / `act.sub` claims): `update_page` and the `close_session` write-through
  answer `{ok: true, held_for_review: true, draft: {...}}` and nothing lands until a reviewer promotes;
  `move_page` / `delete_page` answer `review_required` (a removal cannot be held as a draft). People on
  plain agent-role tokens, admins and `autonomy: auto` skills are unchanged; promoting always lands.
  A runner/agent flow that wrote review-skill pages directly must now propose drafts.
- Round-2 security review (autonomy gate, 2026-10-04): a MACHINE is now gated EVEN WHEN ITS TOKEN IS ADMIN
  (the runner mints its agents' run tokens as admin; the gate used to wave admins through). A machine's
  edit of a SKILL page (`markdown/skills/*`) is held as a draft like a review skill's instance (a run
  cannot write `autonomy: auto` for itself); a skill page that does not parse holds (it failed open).
  `move_page` gates the destination as well as the source; `merge_branch` is refused (`review_required`)
  for a machine when a member page belongs to a review skill; `/ingest` answers `409 review_required`
  for a machine uploading into a review document skill; `write_instance` answers `review_required`.
  `promote_draft` / `promote_changeset` answer `promote_requires_human` to ANY machine token (it was
  write-back drafts only): an agent proposes, a person decides; `discard_draft` / `discard_changeset`
  by a machine only for drafts its own run proposed. `mint_agent_token` no longer inherits `escurel:admin`
  from its minter. `update_page` answers (and the typed clients decode) `held_for_review` / `draft`.
- `secret_ref` is per tenant: `env:` names live in `ESCUREL_SECRET_<TENANT>__*` (`gsm:NAME` reads
  `ESCUREL_SECRET_<TENANT>__<NAME>`), `file:` under `<ESCUREL_SECRET_FILE_DIRS>/<tenant>/`, and
  `ESCUREL_SECRET_ENV_ALLOW` takes `tenant:NAME` (a bare `NAME` stays global). A credential registered
  against the old global names must be re-registered (this is the first release with `secret_ref`).
- SQL sources: the connection-string check parses libpq's grammar (a second host in a URI query, `host =
  x`, host lists, `service=` are judged or refused; keys outside a short allow-list are refused), pins a
  checked host name to its address, and `json_dir` / `parquet_dir` globs must lie under
  `ESCUREL_SQL_FILE_DIRS` (unset = no directory sources; set it on a gateway that serves them).
  `query_instance` enforces the query page's own `acl.read`.

