# Breaking wire changes (this release)

One line per change. Folded into the root `CHANGELOG.md` BREAKING entry and `docs/deploy/README.md`.

- `ESCUREL_WRITE_ACL` defaults to `enforce` (was `off`); an unrecognised value also means `enforce`. A deployment
  that never set it now refuses writes by callers who are neither the instance owner nor an admin (`forbidden`).
  Opt out with `ESCUREL_WRITE_ACL=off`; `log` warns and allows. Skill 0.17.1.

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

- Unknown arguments are REFUSED on every tool: a top-level argument the tool's `inputSchema` does not
  declare (a typo like `limt`, another tool's spelling like `filter` on `list_instances`) answers
  `isError: true`, `issues[{code: "invalid_argument"}]` with a "did you mean" and the valid parameter list,
  instead of being dropped (the call ran with defaults). The documented sibling spellings (`skill` /
  `skill_id`, `from_page` / `from_page_id` / `to_page_id`, `query_id`, `pack_id`) still work. Undeclared
  attribution/lineage arguments (`principal`, `last_written_by`, `run_id`, ...) are refused, not ignored.
  Schema gaps this exposed are declared now: `promote_draft`/`discard_draft` (`decided_by`, `content`,
  `reason`), `delete_page.branch`, `search.page_id`, `admin_quota`/`admin_audit.tenant_id`,
  `register_endpoint.secret_ref`, `tenant_create` (`status`, `quotas`, `embedding_provider`).
- `list_skills` rows now carry what the description always promised for a `rows` backend:
  `backend.{instances: rows|view, key[], filterable[{field,column}], searchable[{field,column}],
  writable_columns[{field,column}], writable_via: "write_back", linked}` (all omitted when not applicable).
  A rows skill may declare `backend.searchable: [<display columns>]`; `search` matches the key, the
  `filterable` and the `searchable` columns (a customer is found by name). `search` with a `skill` filter
  defaults `page_kind` to `instance` (it used to return the skill's own page too; `any` restores that).
  `search` hits omit `similarity` when none was computed (it was `0.0` / `-1.0` sentinels).
- `create_draft` takes `write_back: {patch, base_etag}` as a declared ARGUMENT (the server writes it into
  the frontmatter; `content` is then optional and a minimal row page is built). A patch value outside
  the skill field's kind/enum is refused at draft time (`write_back_invalid_value`) instead of leaving a
  dead draft that blocks the page; the open-draft `conflict` now carries a `suggestion` naming
  `discard_draft`.
- `list_instances` of a skill that does not exist answers `isError` + `unknown_skill` (naming the known
  skills) instead of an empty success. Summary text (`content[0].text`): `expand`/`resolve` of an absent
  page reads "Not found (page: null): ..."; a page names its cursor (`next_cursor=<value>`); a
  refusal carries its whole message and `suggestion` (no mid-sentence "…"); `mint_agent_token` says the
  token was minted (and when it expires) WITHOUT repeating the secret (read `structuredContent.token`).
- List cursors (`next_cursor`) are SIGNED (HMAC; every family: instances, rows, events/inbox, chat,
  drafts). A cursor the server did not issue, or edited, answers `invalid_cursor` — on every paged list,
  in the typed shape (`list_inbox` / `list_events` / `list_messages` used to answer a bare JSON-RPC
  `-32602` carrying a decoder message). The key is random per process: a cursor does not survive a restart
  (restart the listing); replicas of one deployment share `ESCUREL_CURSOR_KEY` so paging works across them.
- `expand` of a ROW page (`instances: rows`) returns each value once: the projected values are in
  `backend_projection.source` (and the frontmatter); the discovered `columns` schema and the raw `rows`
  are behind the new `include_schema: true`. `backend_projection.read_only_fields` names the
  source-owned frontmatter fields (send only your own fields to `update_page`), and `direct_write: false`
  says what `read_only: true` meant (rows change through a `write_back` draft); `read_only` stays for one
  release and is deprecated.
- `describe_backend` is renamed `describe_endpoint` (the old name still answers for one release); for an
  `openapi` endpoint it answers `{kind, base_url, hint}` instead of an `invalid_params` error. `tools/list`
  is sorted by group tag (READ, WRITE, REVIEW, RUNNER, SESSION, ADMIN) and then by name.
- A write whose page has no frontmatter (or none `kind:`) is refused `frontmatter_parse` WITH a
  `suggestion` holding a minimal frontmatter example and the `type:` -> `kind:` rename.
