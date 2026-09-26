# A session key that names no page defeats the page ACL

**Symptom.** Adding a second kind of live session — one whose target is a
personal draft rather than a page (`open_session { draft_id }`) — silently made
`apply_op` and `close_session { commit: false }` open to any caller holding the
session id.

**Cause.** Both handlers gate a caller who is not the session's opener by asking
"may this caller write the session's page?":

```rust
let permitted = match (sessions.page_id_of(&a.session), indexer) {
    (Some(page_id), Some(ix)) => write_acl_off || session_write_allowed(ix, &caller, &page_id, None).await?,
    _ => true,   // no page behind it, or unknown session
};
```

`session_write_allowed` reads the stored page to decide, and decides *allow*
when there is nothing to read — the deliberate fail-open for a page that does
not exist yet. A draft session's key is not a page path, so the read returns
`None` and the gate allows. The `_ => true` arm is the same hole reached the
other way. Neither arm is wrong for pages; both are wrong for a target whose
authority is identity rather than a page ACL.

**Fix.** Match the draft key first and refuse, before either page arm:

```rust
(Some(key), _) if crate::session::draft_id_of_key(&key).is_some() => false,
```

Only the draft's author (admin aside) may apply ops to it or discard it, which
is the same bar `open_session` sets. Pinned by
`draft_sessions::a_session_id_does_not_let_another_subject_touch_a_personal_draft`.

**How to recognise it next time.** Any authorisation that resolves a handle to a
*resource path* and then asks a question about that path will answer "allowed"
for a handle whose path it cannot resolve. When you add a second kind of thing
behind the same handle, audit every place that maps the handle to a path — the
ones that fail open are the ones that look like they are already checking.
