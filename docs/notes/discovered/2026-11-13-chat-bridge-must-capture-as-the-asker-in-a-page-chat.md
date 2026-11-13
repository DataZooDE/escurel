# A chat turn is answered only in a page-bound group, captured by the asker themselves

**Symptom.** A client outside the explorer captures a `chat` event, and no assistant turn
ever appears in the chat. Two shapes look right and fail. One uses an opaque chat group,
such as the Teams conversation id. The other captures through a delegated token
(`mint_agent_token`, or a service identity with the asker recorded in provenance).

**Why.** The runner builds a `ChatContext` only when `provenance.chat_group_id` starts with
`page-chat:` (`escurel-runner-core/src/trigger.rs`). Any other trigger is run as a plain
event, and only a chat run posts a reply. The asker is `provenance.captured_by` only
when `captured_via` is absent. A delegated capture stamps `captured_via: agent:<…>`, so the
asker is unknown and the chat run refuses rather than answer with someone else's rights.
The gateway's reply rule (#785) accepts the run's assistant turn only in the page chat that
the root event names.

**Fix (the chat bridge, #824).** Each platform chat becomes a `conversation` instance the
person owns (`teams-<hash>`). The turn goes into its group,
`page-chat:v1:<sha256(asker)>:<page>`. The bridge holds the signing key the gateway trusts,
as a minted-mode runner does, and signs a short-lived, group-less token whose `sub` is the
asker. It captures with that token, so `captured_by` is the asker and there is no
`captured_via`. `mint_delegation` does not help here: its audience is another agent, and the
gateway rejects that purpose.

**Recognise it next time.** You are tempted to name a chat group after an external
conversation id, or to "act for" a user with an agent token, in order to start a chat run.
