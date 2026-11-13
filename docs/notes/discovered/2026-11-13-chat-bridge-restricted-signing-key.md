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

**Decision (now).** The bridge holds a key of its own. The gateway lists its kid
in `ESCUREL_AUTH_BRIDGE_KID`; `OidcVerifier` routes any token under that kid
through `bridge_context` (`crates/escurel-auth/src/verifier.rs`), which bounds
it regardless of its claims:

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
