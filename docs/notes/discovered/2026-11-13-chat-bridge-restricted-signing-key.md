# The chat bridge signs with its own, gateway-restricted key

**Symptom (crew review of #824, F1).** `escurel-chat-bridge` was configured with
the RSA private key the gateway trusts for minted-mode runners. Its code signed
only `sub` + no groups + 900 s, but that was the bridge's choice: anyone who got
the key out of the internet-facing bridge could sign any subject, with
`escurel:admin`, for any TTL. The tokens also carried no marker, so an event
the bridge captured for a person read exactly like one they typed themselves
(`captured_by: <person>`, no `captured_via`), and a comment claimed the token
was "exactly the person, as their own token would present them" while it
silently dropped their IdP groups.

**Decision (now).** The bridge holds a key of its own. The gateway names the
bridge's issuer in `ESCUREL_AUTH_BRIDGE_ISSUER` and pins it to the kid in
`ESCUREL_AUTH_BRIDGE_KID`; `OidcVerifier` routes every token verified against
that trust entry through `bridge_context`
(`crates/escurel-auth/src/verifier.rs`), which bounds it regardless of its
claims:

- `exp - iat` and `exp - now` at most 900 s, else `401`;
- `agent`, `skill`, `run_id`, `root_event_id`, `purpose`, `act` present → `401`
  (a bridge never mints them, so their presence means the key is misused);
- `bridge_channel` must be a plain channel name → actor `bridge:<channel>`;
- role is always `Agent`, never operator, groups always empty.

The actor flows into `captured_via` like any other actor, so the gateway stamps
`captured_via: bridge:msteams`. The runner's chat asker rule
(`escurel-runner-core/src/trigger.rs`) and the gateway's page-chat reply rule
(`escurel-index/src/acl.rs`, `may_reply_in_page_chat`) accept exactly
`escurel_types::bridge::is_bridge_via` and no other via. A bridge caller counts
as a machine caller (it has an actor), so the autonomy gate applies to its
writes, which is the conservative side.

Consequence to know: a bridge identity carries **no IdP groups**. ACLs that
grant by a token-claim group do not reach a person through Teams; ownership,
`group_members` groups and `public` still do.

**End state (follow-up, not built).** A gateway tool `mint_user_token{subject}`
callable only by a service role (`escurel:bridge`), which resolves the person's
groups server-side and signs with the gateway's own identity. Then the bridge
holds no signing key at all, the group gap closes, and the restriction lives in
one place. The kid restriction stays useful as the transport for that role's
own credential.

**Recognise it next time.** Any service that "acts as a person" by signing
tokens: ask whose key it holds and what the *gateway* — not the service —
limits a token under that key to.

## Follow-up: a kid-scoped restriction does not restrict whoever serves the JWKS

**Symptom (crew security review of PR-2, F1).** The first cut keyed the
restriction on the token's `kid`: `if bridge_kids.contains(&kid) {
bridge_context(..) }`, checked after the key was fetched from the issuer's
JWKS. But the bridge's trust entry fetches its JWKS *from the bridge itself*
(and the deploy guide showed that URL as plain http). Whoever controls that
response — a compromised bridge host, or anyone on the network path — could
publish a second key under kid `evil`, sign `{iss: <bridge>, roles:
[escurel:admin, escurel:operator]}` with it, and the gateway would refresh on
the unknown kid, verify the signature, find `evil` not in the list, and take
the **unrestricted** path. The restriction meant to contain a compromised
bridge was bypassed by exactly a compromised bridge.

**Fix.** The restriction belongs to the trust entry. `ESCUREL_AUTH_BRIDGE_ISSUER`
marks one configured issuer as the bridge; every token verified against it is
a bridge token whatever its kid. The kid list stays as a pin, checked before
any key fetch: a token from the bridge entry under an unlisted kid is refused,
and so is a token from another entry under a bridge kid. The gateway refuses to
start when a bridge kid has no bridge issuer, the bridge issuer is not a trusted
issuer, or the bridge issuer has no kid. Regression test:
`crates/escurel-auth/tests/oidc.rs` `bridge_entry::a_rogue_kid_in_the_bridge_jwks_with_admin_operator_and_run_claims_is_refused`.
The deploy guide now says https for the bridge JWKS.

**Recognise it next time.** A restriction keyed on something the restricted
party chooses (a kid in a key set it publishes, a claim it signs, a header it
sends) restricts nobody but the honest. Key the restriction on what the
*verifier* chose — the trust entry the token was verified against.
