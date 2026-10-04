# Breaking wire changes (this release)

One line per change. Stream C turns this into the root `CHANGELOG.md` BREAKING entry.

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
