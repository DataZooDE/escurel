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

