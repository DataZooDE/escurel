# The egress policy refuses your own local upstream (by design)

**Symptom.** After the REST/MCP connectors landed, tests and demos that point a skill at
`http://127.0.0.1:<port>` fail with a worded refusal (`egress refused: ...`) even though the upstream is
up. `validate_endpoints` reports `refused`, not `unreachable`.

**Why.** Outbound calls from the gateway go through `crates/escurel-server/src/egress.rs`: https only,
public addresses only (loopback, private, link-local and the cloud metadata address are refused), DNS is
resolved once and the connection is pinned to that answer, no redirects, capped size/time/rate. Without
that, a tenant's skill page (or a poisoned page the agent reads) could make the gateway call internal
services (SSRF).

**Fix.** Local development and tests opt in explicitly: `ESCUREL_EGRESS_ALLOW_LOOPBACK=1` for a process,
or `ConfigOverrides.egress` with `allow_loopback: true` in a Rust test
(`escurel_test_support::EgressPolicy`). Never set it in production. `demo/run.sh` sets it for the demo
gateway only. The old tests that talked to a loopback upstream (`remote_backend_tools`,
`crm_demo_backends`, `escurel-client` `typed_tools`) were updated this way.

**Recognise it.** A remote-backend test that used to pass fails right after a change to `egress.rs`, with
`refused` in the message and no request ever reaching the upstream (its request counter stays 0).
