# List cursors are HMAC-signed: a forged, edited or pre-restart cursor is refused

**Symptom.** The second MCP usability review forged a `list_instances` cursor by hand (`r1.` + base64 of a
key) and the gateway happily resumed from it: the envelope was opaque only by convention. Separately,
a bad cursor on `list_inbox` / `list_events` came back as a raw JSON-RPC `-32602 "utf-8: invalid utf-8
sequence"` instead of the `invalid_cursor` issue the other list tools answer.

**Fix.** Every cursor family (instances, events, chat, rows, drafts, changesets, messages) goes through
one codec, `crates/escurel-index/src/cursor.rs` (`seal` / `unseal`):
`base64url("<sort-key>|<row-id>") . base64url(HMAC-SHA256 tag)`. A token the server did not issue
answers `invalid_cursor` with the suggestion "restart without `cursor`".

**The consequence operators must know.** The signing key is random per process unless
`ESCUREL_CURSOR_KEY` is set (`docs/deploy/env.md`). So: a cursor does not survive a restart, and a cursor
issued by one replica is refused by another, unless every replica of a deployment shares the SAME
`ESCUREL_CURSOR_KEY`. A client that pages across a rolling restart sees `invalid_cursor` and must restart
its listing; that is by design, not a bug.

**How to recognise it.** `invalid_cursor` right after a deploy, or only on some replicas behind a load
balancer. Check the key is set identically everywhere; never put it in a page or a log.

**Tests.** `crates/escurel-server/tests/suite/mcp_ax_round2.rs` (forged / tampered cursors on every
list tool answer `invalid_cursor`).

## Update 2026-10-10: the tag is bound to the list, and the key has a minimum

The first version signed only the payload, so a cursor issued for one skill opened on another
(`list_instances` returned a silently empty page) and a cursor from one list type opened on another.
The tag now also covers a scope (`instances:<tenant>:<skill>`, `events:<tenant>`,
`chat:<tenant>:<group>`, `rows:<tenant>:<skill>`, `remote:<tenant>:<skill>`, `drafts|changesets|branches:<tenant>`),
length-prefixed so a byte cannot move between scope and payload. Replaying a cursor anywhere but where it was
issued is `invalid_cursor`. `ESCUREL_CURSOR_KEY` shorter than 32 bytes stops the server at boot
(`invalid value for ESCUREL_CURSOR_KEY: "<redacted>"`). Tests: `cursor::tests` in `escurel-index`,
`a_cursor_is_bound_to_the_list_it_was_issued_for` and `a_short_cursor_key_stops_the_server_at_boot`.
