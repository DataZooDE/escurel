# A refused read decoded as an empty success

**Symptom.** `escurel-client`'s `query_instance` / `list_instances` returned `Ok(<empty>)` for a call the
gateway had refused (peacock's `acl_denial_is_a_typed_error_not_a_partial_read` failed: the caller saw
zero rows after an access denial).

**Cause.** The MCP-usability stream made a domain refusal a normal `tools/call` result: `isError: true`,
`structuredContent {ok: false, issues: [...]}`. `call_typed` unwrapped `structuredContent` and ran
`serde_json::from_value` into the response type regardless of `isError`. Every read-family response type
has all-`#[serde(default)]` fields, so `{ok: false, issues}` is a valid, empty `Ok`. The same blind read
existed in the echo/Gemini harness clients, the test-support client, the VS Code client and the Dart one.

**Fix.** One reader, `escurel_types::call_result::{payload_of, refusal_of, unwrap_call_result}`;
`Error::Refused`; `call_typed` / `call` strict; `call_typed_outcome` / `call_outcome` for the tools whose
answer IS an `ok`/`issues` (writes, `validate`) or a report (`rebase_pack`, Dart `validate_bindings`,
`validate_endpoints`). `crates/escurel-types/tests/no_blind_structured_content.rs` fails when a crate
reads the field itself.

**Recognise it next time.** A read that "succeeds" with nothing in it right after you changed what the
caller may see; a typed response whose fields all default; a new consumer that mentions `structuredContent`.
