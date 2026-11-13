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
4. **Acts for that person**: it signs a 15-minute token for the principal with the bridge's
   **own** key (`ESCUREL_BRIDGE_AUTH_SIGNING_KEY`, kid `ESCUREL_BRIDGE_AUTH_KID`). Never hand the
   bridge the gateway's main signing key or a runner's: the bridge is internet-facing, and that
   key can sign any subject as admin. The gateway names the bridge's issuer in
   `ESCUREL_AUTH_BRIDGE_ISSUER` (pinned to its kid, `ESCUREL_AUTH_BRIDGE_KID`) and restricts every
   token from it, whatever it says and whichever kid it names: at most 900 s,
   never admin or operator, no roles or groups, no agent or run claims. The event reads
   `captured_by: <person>` and `captured_via: bridge:msteams` (stamped by the gateway), and the
   run answers with the person's read rights **minus their IdP groups**: the bridge does not know
   them, so a page shared only with a token-claim group is not visible through Teams (ownership,
   `group_members` groups and `public` still apply).
5. Creates the person's **conversation page** for this Teams chat once
   (`markdown/instances/conversation/teams-<hash>.md`, owner-private), appends the message to its
   page-bound chat group (`msg_id` `msteams:<hash of conversation + activity id>`), and captures a
   `chat` event naming it (`event_id` `msteams-<same hash>`, so a retry stores no second event).
6. **Attachments**: each file is downloaded (at most `ESCUREL_BRIDGE_ATTACHMENT_MAX_BYTES`) and
   uploaded through `ingest_upload` as the person. A URL on the connector's own origin (scheme,
   host and port of the token-bound `serviceUrl`) is fetched with the bot's bearer; any other URL
   must be https on a host in `ESCUREL_BRIDGE_TEAMS_ATTACHMENT_HOSTS` (SharePoint and Teams media
   by default), must resolve only to public addresses, is fetched pinned to those addresses without
   the bearer, and is not redirected. So an activity cannot make the bridge read an internal
   endpoint (`http://127.0.0.1:8080/metrics`, a metadata address) into escurel. The message gets one line per attachment:
   the page it became, or `not ingested: <why>` (no document skill accepts the type, the person
   may not create in it, the download failed).
7. **Typing**: while no answer exists, a typing activity every `ESCUREL_BRIDGE_TYPING_INTERVAL`.
8. **One reply**: when the run's assistant turn (`run:<id>:reply`) appears in the group, the bridge
   claims the reply in its state file, then posts it once, as a reply to the activity. If no answer
   arrives within `ESCUREL_BRIDGE_REPLY_TIMEOUT` of the message's arrival it says so instead
   (*I couldn't get an answer in time. Please ask again.*), once.

A gateway outage (a stop-first deploy) does not lose a turn: the bridge retries it with backoff
(starting at `ESCUREL_BRIDGE_POLL_INTERVAL`, doubling, at most 30 s) until it is answered or its
deadline — arrival plus `ESCUREL_BRIDGE_REPLY_TIMEOUT`, stored with the turn — passes, and a sweep
every 30 s picks up any open turn nothing is working on. The timeout notice goes to Teams
directly, so it is sent even if the gateway never came back.

A restarted bridge resumes every unanswered turn from its state file, with the deadline it
already had. Each step is idempotent, so a restart in the middle sends the reply once. A crash between claiming and posting a reply
loses that reply rather than risking a second one. For the same reason a post is retried only
when it provably never left (the connector token could not be fetched, no connection was made,
or the connector answered `429`). A timeout, a reset after sending or a `5xx` may mean the
connector delivered it, so the turn closes as `send_uncertain` (logged) and is not posted again.

Only 1:1 (`personal`) chats are answered. A page chat is private to one person, and a reply in a
group chat or channel would show one member's answer to everyone in it.

## The tenant needs

- the chat corpus: the `conversation` and `chat` skills, shipped as files in
  [`examples/chat/seed/skills/`](../../examples/chat/seed/skills/) (byte for byte what
  `escurel_runner_workflow::corpus::chat_corpus()` holds; a unit test keeps them equal). Install
  them once with an admin token:

  ```sh
  export ESCUREL_SERVER=https://escurel.example.com ESCUREL_TOKEN=<admin bearer for the tenant>
  for s in conversation chat; do
    escurel page update "markdown/skills/$s.md" < "examples/chat/seed/skills/$s.md"
  done
  escurel skill list            # lists conversation and chat
  ```

  (A dev gateway can instead boot with `ESCUREL_SEED_DIR=examples/chat/seed`.) Or bring your own
  skills and name them with `ESCUREL_BRIDGE_CONVERSATION_SKILL` / `ESCUREL_BRIDGE_CHAT_SKILL`;
- a **minted-mode runner** (chat runs need run tokens: `ESCUREL_RUNNER_AUTH_ISSUER` +
  `ESCUREL_RUNNER_AUTH_SIGNING_KEY`, see the *Chat runs* section of
  [`references/11`](../../.claude/skills/escurel-platform/references/11-event-driven-agents.md));
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

## The bridge's signing key

Generate the bridge its own RSA key (never the gateway's `ESCUREL_AUTH_SIGNING_KEY` or a
runner's) and keep it in Secret Manager:

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out escurel-chat-bridge.pem
```

The bridge serves the **public** half as a JWKS at `GET /.well-known/jwks.json` (one RS256 key under
`ESCUREL_BRIDGE_AUTH_KID`; the private key never leaves the process). The gateway fetches it from
there, so the bridge must be reachable from the gateway on its listen port. Do not expose that
path publicly beyond what `/api/messages` already is; it holds nothing secret.

## Gateway side

Add the bridge as one more trusted issuer, mark it as the bridge, and pin its kid. `<N>` is the next free number in
the gateway's contiguous `ESCUREL_AUTH_OIDC_ISSUER_2.._N` sequence (a gap stops the scan; use `2`
if there is no second issuer yet). The issuer string is any URL you choose; it only has to equal
the bridge's `ESCUREL_BRIDGE_AUTH_ISSUER`. The extra issuer shares the gateway's audience
(`ESCUREL_AUTH_OIDC_AUDIENCE`, which must equal the bridge's `ESCUREL_BRIDGE_AUTH_AUDIENCE`,
default `escurel`) and tenant claim.

```sh
# on the gateway
ESCUREL_AUTH_OIDC_ISSUER_2=https://chat-bridge.escurel.internal
ESCUREL_AUTH_JWKS_URI_2=https://escurel-chat-bridge:3978/.well-known/jwks.json
ESCUREL_AUTH_BRIDGE_ISSUER=https://chat-bridge.escurel.internal   # = ESCUREL_AUTH_OIDC_ISSUER_2
ESCUREL_AUTH_BRIDGE_KID=escurel-chat-bridge-1
```

Every token verified against that issuer is restricted (at most 900 s, never admin or operator, no
roles or groups, no agent or run claims) whatever it says, and a token from it under any kid other
than `ESCUREL_AUTH_BRIDGE_KID` is refused. The gateway refuses to start when
`ESCUREL_AUTH_BRIDGE_KID` is set without `ESCUREL_AUTH_BRIDGE_ISSUER`, when the bridge issuer is not
one of its trusted issuers, or when it has no kid.

**Fetch the bridge's JWKS over https** (or a path nobody else can write to, such as a
loopback-only sidecar). Whoever controls that response decides which keys sign as the bridge. With
the restriction bound to the issuer and pinned to the kid, a forged key set buys no more than the
bridge key itself would, but on plain http anyone on the network path gets that much. The gateway
reads JWKS over HTTP(S) only; it has no static-file JWKS option. Rotating the bridge key: generate a new key,
deploy the bridge with it and a new `ESCUREL_BRIDGE_AUTH_KID`, and set
`ESCUREL_AUTH_BRIDGE_KID=<old>,<new>` until the old one is gone (the gateway re-reads the JWKS on an
unknown listed kid). The decision and the intended end state (a gateway-side mint, so the bridge holds no
signing key at all) are in
`docs/notes/discovered/2026-11-13-chat-bridge-restricted-signing-key.md`.

## Environment

The bridge's variables are the *Bridge* table of [`env.md`](env.md). A complete single-tenant
deployment (the recommended internal bot):

```sh
# escurel
ESCUREL_BRIDGE_GATEWAY_URL=http://escurel:8080
ESCUREL_BRIDGE_TENANT=acme
ESCUREL_BRIDGE_AUTH_ISSUER=https://chat-bridge.escurel.internal   # = the gateway's ESCUREL_AUTH_BRIDGE_ISSUER (and _OIDC_ISSUER_<N>)
ESCUREL_BRIDGE_AUTH_KID=escurel-chat-bridge-1                     # = the gateway's ESCUREL_AUTH_BRIDGE_KID
ESCUREL_BRIDGE_AUTH_SIGNING_KEY_FILE=/run/secrets/escurel-chat-bridge.pem
ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE=user:{aadObjectId}              # = the person's own token `sub`
ESCUREL_BRIDGE_STATE_PATH=/data/escurel-chat-bridge.duckdb        # persistent volume
ESCUREL_BRIDGE_LISTEN=0.0.0.0:3978
# Microsoft
ESCUREL_BRIDGE_TEAMS_APP_ID=<Application (client) id>
ESCUREL_BRIDGE_TEAMS_APP_PASSWORD_FILE=/run/secrets/teams-app-password
ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS=<entra tenant id>
ESCUREL_BRIDGE_TEAMS_TOKEN_URL=https://login.microsoftonline.com/<entra tenant id>/oauth2/v2.0/token
```

Without `ESCUREL_BRIDGE_TEAMS_TOKEN_URL` a single-tenant bot gets no connector token, and the only
symptom is `token endpoint answered 400` in the log. Logs are JSON on stdout
(`app: escurel-chat-bridge`). `GET /healthz` is dependency-free.

## First-message smoke check

1. `curl -s http://escurel-chat-bridge:3978/healthz` → `ok`, and
   `curl -s http://escurel-chat-bridge:3978/.well-known/jwks.json` lists one key whose `kid` is
   `ESCUREL_AUTH_BRIDGE_KID`.
2. `escurel skill list` shows `conversation` and `chat`; `escurel runtime list` shows a runner
   `online` (minted mode).
3. In Teams, open a 1:1 chat with the bot and send *hello*. Within a few seconds the bot shows
   typing, then answers once.
4. If nothing comes back, read the bridge log in order: `inbound request refused` (the Bot
   Framework token: app id, endpoint URL), no `turn captured` (the gateway refused the bridge
   token: issuer, JWKS URL, kid, audience), `turn captured` but no reply (the runner: minted mode,
   the `chat` skill), `token endpoint answered 400` (`ESCUREL_BRIDGE_TEAMS_TOKEN_URL`). The captured
   turn is a `chat` event (`list_events {label_skill: chat}` as an admin) with
   `captured_by: <the person>` and `captured_via: bridge:msteams`.

## Not verified without a real tenant

The integration test (`crates/escurel-chat-bridge/tests/teams_roundtrip.rs`) runs a real gateway,
runner and bridge against local stand-ins for Microsoft's metadata, JWKS, token endpoint and
connector. These behaviours follow Microsoft's documentation and have not been checked against a
real tenant:

- the `serviceurl` claim name and value on Teams channel tokens;
- the JWKS `endorsements` (channel ids per key): the bridge requires `msteams` when a key lists
  them, and accepts a key that lists none;
- Bot Framework Emulator tokens (a different issuer), which the bridge does not accept;
- inline-image downloads needing the connector bearer, and whether Teams file links redirect
  (the bridge does not follow redirects; add the final host to the allow-list if one does);
- the single-tenant token endpoint.
