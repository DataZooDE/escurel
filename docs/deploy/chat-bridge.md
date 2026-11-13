# Teams chat bridge (`escurel-chat-bridge`)

People ask escurel questions in Microsoft Teams. The bridge turns a 1:1 Teams
message into the same page-chat turn the explorer and VS Code chat produce,
and sends the runner's answer back as one reply.

```
Teams ──POST /api/messages──▶ escurel-chat-bridge ──/mcp (as the asker)──▶ gateway
  ▲   (Bot Framework JWT)        │  conversation page + user turn            │
  │                              │  + `chat` event                           ▼
  └──── one reply ◀── connector ◀┴── polls the chat group ◀── runner answers in chat mode
```

It is a separate process so the gateway stays free of platform automation. It ships in the
runner image (`Dockerfile.runner`) as `/usr/local/bin/escurel-chat-bridge` and is off unless you
start it.

## What it does with a message

1. **Verifies** the request: the `Authorization` bearer is an RS256 JWT signed by a key from the
   Bot Framework OpenID metadata (`ESCUREL_BRIDGE_TEAMS_OPENID_METADATA_URL`), `iss` =
   `ESCUREL_BRIDGE_TEAMS_ISSUER`, `aud` = `ESCUREL_BRIDGE_TEAMS_APP_ID`, unexpired (5 min skew),
   and its `serviceurl` claim equals the activity's `serviceUrl`. The activity's `channelId` must
   be `msteams`, and when the JWKS lists the signing key's `endorsements` they must include
   `msteams`. Anything else is `401` and nothing is captured.
   Only a person in an Entra tenant listed in `ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS`
   (`conversation.tenantId`, else `channelData.tenant.id`) in a conversation whose
   `conversationType` is explicitly `personal` is answered; anything else is answered `200` and
   ignored.
2. **Dedupes** on the conversation id plus the activity id, in its state file
   (`ESCUREL_BRIDGE_STATE_PATH`). A redelivered activity is answered `200` and ignored.
3. **Maps the person**: `ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE` (required, no default) with the
   sender's Entra object id (`{aadObjectId}`), Teams user id (`{userId}`) or Entra tenant
   (`{tenantId}`). The bridge refuses to start on an unknown placeholder (a misspelt
   `{aadObjectID}` would stay a literal) or a template naming neither `{aadObjectId}` nor
   `{userId}`: a constant or tenant-only template maps every sender to one subject, who then share
   each other's private pages. The result must be the `sub` the person's own token from the
   gateway's **primary issuer** carries. If that issuer is Entra itself, note that Entra's `sub` is
   pairwise per app and is *not* the object id; use an issuer (the substrate IdP) whose `sub` is
   derived from the object id, or the bridge identity is a different principal from the person's
   web identity. A sender without the named field gets no answer.
4. **Acts as that person**: it signs a 15-minute token for the principal with the key in
   `ESCUREL_BRIDGE_AUTH_SIGNING_KEY` (no groups, no admin). The gateway must trust that key, the
   same trust a minted-mode runner has. The event therefore reads `captured_by: <person>` with no
   `captured_via`, and the run answers with the person's read rights.
5. Creates the person's **conversation page** for this Teams chat once
   (`markdown/instances/conversation/teams-<hash>.md`, owner-private), appends the message to its
   page-bound chat group (`msg_id` `msteams:<hash of conversation + activity id>`), and captures a
   `chat` event naming it (`event_id` `msteams-<same hash>`, so a retry stores no second event).
6. **Attachments**: each file is downloaded (at most `ESCUREL_BRIDGE_ATTACHMENT_MAX_BYTES`) and
   uploaded through `ingest_upload` as the person. The message gets one line per attachment:
   the page it became, or `not ingested: <why>` (no document skill accepts the type, the person
   may not create in it, the download failed).
7. **Typing**: while no answer exists, a typing activity every `ESCUREL_BRIDGE_TYPING_INTERVAL`.
8. **One reply**: when the run's assistant turn (`run:<id>:reply`) appears in the group, the bridge
   claims the reply in its state file, then posts it once, as a reply to the activity. If no answer
   arrives within `ESCUREL_BRIDGE_REPLY_TIMEOUT` it says so instead.

A restarted bridge resumes every unanswered turn from its state file. Each step is idempotent,
so a restart in the middle sends the reply once. A crash between claiming and posting a reply
loses that reply rather than risking a second one.

Only 1:1 (`personal`) chats are answered. A page chat is private to one person, and a reply in a
group chat or channel would show one member's answer to everyone in it.

## The tenant needs

- the chat corpus (`conversation` + `chat` skills,
  `escurel_runner_workflow::corpus::chat_corpus()`), or your own skills named by
  `ESCUREL_BRIDGE_CONVERSATION_SKILL` / `ESCUREL_BRIDGE_CHAT_SKILL`;
- a **minted-mode runner** (chat runs need run tokens, references/11 *Chat runs*);
- for attachments, a `document` skill that accepts the type and grants its `create` to the person
  (e.g. `owner_field: owner`, `acl: {create: [owner]}`).

## Register the bot in Entra and Azure

1. **App registration** (Entra admin center → App registrations → New registration). Pick
   *single tenant* for an internal bot. Note the **Application (client) id**, which becomes
   `ESCUREL_BRIDGE_TEAMS_APP_ID`.
2. **Client secret** (Certificates & secrets → New client secret). Store it in GCP Secret Manager
   and inject it as `ESCUREL_BRIDGE_TEAMS_APP_PASSWORD` (or mount it and point
   `ESCUREL_BRIDGE_TEAMS_APP_PASSWORD_FILE` at it).
3. **Azure Bot resource** (Azure portal → Create → Azure Bot). Use the app id from step 1 and the
   same tenancy type. Under *Configuration*, set the **messaging endpoint** to
   `https://<public host>/api/messages`. The bridge listens on `ESCUREL_BRIDGE_LISTEN`
   (default `0.0.0.0:3978`), behind TLS termination.
4. **Channels** → add **Microsoft Teams**.
5. **Teams app**: a manifest with `bots[].botId` = the app id and scope `personal`. Upload it in
   the Teams admin center (or sideload it for a test).
6. A **single-tenant** bot gets its connector token from its own tenant: set
   `ESCUREL_BRIDGE_TEAMS_TOKEN_URL=https://login.microsoftonline.com/<entra tenant id>/oauth2/v2.0/token`.
   The default (`…/botframework.com/…`) is the multi-tenant endpoint.

## Environment

The bridge's variables are the *Bridge* table of [`env.md`](env.md). The minimum is
`ESCUREL_BRIDGE_GATEWAY_URL`, `ESCUREL_BRIDGE_TENANT`, `ESCUREL_BRIDGE_AUTH_ISSUER` (+ `_KID`),
`ESCUREL_BRIDGE_AUTH_SIGNING_KEY`, `ESCUREL_BRIDGE_TEAMS_APP_ID`,
`ESCUREL_BRIDGE_TEAMS_APP_PASSWORD`, `ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS` and
`ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE`.
Keep `ESCUREL_BRIDGE_STATE_PATH` on the persistent volume (`/data`). Logs are JSON on stdout
(`app: escurel-chat-bridge`). `GET /healthz` is dependency-free.

## Not verified without a real tenant

The integration test (`crates/escurel-chat-bridge/tests/teams_roundtrip.rs`) runs a real gateway,
runner and bridge against local stand-ins for Microsoft's metadata, JWKS, token endpoint and
connector. These behaviours follow Microsoft's documentation and have not been checked against a
real tenant:

- the `serviceurl` claim name and value on Teams channel tokens;
- the JWKS `endorsements` (channel ids per key): the bridge requires `msteams` when a key lists
  them, and accepts a key that lists none;
- Bot Framework Emulator tokens (a different issuer), which the bridge does not accept;
- inline-image downloads needing the connector bearer;
- the single-tenant token endpoint.
