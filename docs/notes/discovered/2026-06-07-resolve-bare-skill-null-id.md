# `resolve("[[<skill>]]")` returns a null `parsed.id` the client couldn't decode

## Symptom

Resolving a **bare-skill** wikilink (e.g. `[[customer]]`, no `::id`
segment) over `/mcp` failed client-side with:

```
Decode("resolve: invalid type: null, expected a string")
```

even though the gateway answered `200 OK` with a valid `page`. Resolving
a fully-qualified `[[skill::id]]` (e.g. `[[customer::acme]]`) worked,
which is why the existing `escurel-client` roundtrip tests never tripped
it.

## Cause

`tool_resolve` in `escurel-server` echoes the parsed wikilink as
`parsed: { skill, id, anchor, version, alias }`. For a bare-skill link
there is no id segment, so the wire emits `parsed.id: null`. The
`escurel_types::WikilinkParsed` struct deserialized `skill` and `id` as
plain `String` (no null tolerance), so serde rejected the `null`.

The context packager (#150) resolves the `label_skill` exactly as the
spec prescribes — `resolve("[[<label_skill>]]")` — which is always a
bare skill name, so it hit this on the first real call.

## Fix

Add `#[serde(deserialize_with = "null_as_default")]` to
`WikilinkParsed::skill` and `::id` (the struct already used that helper
for `anchor`/`version`/`alias`). A `null` segment now decodes to the
empty string instead of failing.

## Recognise it next time

Any new `escurel-types` field that mirrors an MCP wire value which the
server can legitimately emit as `null` (proto3 has no nullable string, so
absent = empty, but the JSON wire often sends explicit `null`) must
tolerate `null` via `null_as_default`. If a decode fails with
`invalid type: null, expected a string` against a 200 response, this is
the class of bug.
