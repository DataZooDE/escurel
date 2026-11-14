# Teams chat bridge (`escurel-chat-bridge`)

People ask escurel questions in Microsoft Teams — in a 1:1 chat with the bot, or by
@mentioning it in a group chat or a channel thread. The bridge turns the message into the same
page-chat turn the explorer and VS Code chat produce, and sends the runner's answer back as one
reply.

```
Teams ──POST /api/messages──▶ escurel-chat-bridge ──mint_user_token (escurel:bridge)──▶ gateway ──▶ group directories
  ▲   (Bot Framework JWT)        │ ◀── the turn's token (person or group:<g>, its groups) ┘    (Graph / Google / Okta)
  │                              │  conversation page + user turn + `chat` event, with that token
  └──── one reply ◀── connector ◀┴── polls the chat group ◀── runner answers in chat mode
```

The bridge holds **no signing key**: the gateway mints each turn's token for it, decides who the
turn acts as and which groups it carries, and refuses a shared chat no admin has connected to a
group.

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
   (`conversation.tenantId`, else `channelData.tenant.id`) is answered, in a conversation whose
   `conversationType` is `personal`, or `groupChat` / `channel` when the message @mentions the bot
   (a `mention` entity naming it; the `<at>…</at>` text is stripped). Anything else — a missing
   type included — is answered `200` and ignored.
2. **Dedupes** on the conversation id plus the activity id, in its state file
   (`ESCUREL_BRIDGE_STATE_PATH`). A redelivered activity is answered `200` and ignored.
3. **Gets the turn's token from the gateway** (`mint_user_token`, on the bridge's own
   `escurel:bridge` credential, `ESCUREL_BRIDGE_GATEWAY_TOKEN[_FILE]`). It sends what it knows of the
   sender: the Entra object id and tenant, the Teams user id, and the email / UPN from the Bot
   Framework conversation-members API (`GET {serviceUrl}/v3/conversations/{id}/members/{userId}`,
   with the bot's own connector token — no extra Microsoft permission). The gateway decides:
   - **who**: `ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE` (gateway side) with `{aadObjectId}`,
     `{userId}`, `{tenantId}`. The gateway refuses to start on an unknown placeholder, a template
     naming neither `{aadObjectId}` nor `{userId}` (every sender would share one subject), or one
     starting `group:`. The result must be the `sub` the person's own token from the gateway's
     **primary issuer** carries. If that issuer is Entra itself, note that Entra's `sub` is pairwise
     per app and is *not* the object id; use an issuer (the substrate IdP) whose `sub` is derived
     from the object id. A sender without the named field (or with no object id) gets no answer.
   - **which groups**: the union over the gateway's group directories (*Group directory* below).
   - **as whom, in a shared chat**: a group chat or channel acts as the group principal
     `group:<g>` an admin mapped it to (*Group chats and channels* below), the person as the asker.
     An unmapped chat, a sender who is not a member of `<g>`, or a directory that cannot be reached
     gets **one polite reply** and no run.
4. **Acts with that token**: at most 900 s, re-minted after half of it, never admin, and confined
   to the few calls a turn needs (below). The event reads `captured_by: <person or group:<g>>`,
   `captured_via: bridge:msteams`, `captured_groups: [...]` and, for a group principal,
   `asked_by: <person>` — all stamped by the gateway — and the run answers with exactly that
   subject and those groups, so a page the turn may not read never reaches it.
5. Creates the subject's **conversation page** for this Teams chat once
   (`markdown/instances/conversation/teams-<hash>.md`, owned by the person in a 1:1 chat and by
   `group:<g>` in a shared one, so a group chat has one history for its group), appends the message to its
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
   claims the reply in its state file, then posts it once: in a 1:1 chat as a reply to the
   activity, in a group chat as an activity in the conversation, in a channel as a reply to the
   thread's root (`;messageid=<root>` in the conversation id). If no answer
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

A shared chat never acts as one member: a reply there is read by everyone in it, so it acts as the
group an admin chose, with that group's access only.

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

## The bridge's credential

The bridge authenticates to the gateway with its own service credential: a token from the
gateway's IdP whose roles include `escurel:bridge` (`ESCUREL_AUTH_BRIDGE_ROLE_VALUE` renames it),
for the bridge's own subject (e.g. `svc:chat-bridge`), never acting for anyone (`act`, `run_id` or
`agent` claims strip the role). It may call `mint_user_token` and nothing an admin alone may do.
Store it in Secret Manager and mount it as `ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE`. If it leaks, an
attacker can get turn tokens for people in the configured directories — confined to the chat
relay's few calls — so rotate it like any service credential.

## Gateway side

```sh
# on the gateway
ESCUREL_AUTH_SIGNING_KEY="$(cat /run/secrets/escurel-signing.pem)"  # the gateway mints the turns
ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE=user:{aadObjectId}           # = the person's own token `sub`
ESCUREL_AUTH_GROUP_DIRECTORIES=graph                                # any of graph,google,okta
# + the keys of each listed directory (below)
# ESCUREL_AUTH_BRIDGE_CHAT_SKILL=chat                               # = the bridge's ESCUREL_BRIDGE_CHAT_SKILL
```

The gateway refuses to start when the template is set without a signing key, with
`ESCUREL_AUTH_GROUPS_CLAIM` other than `roles`, without `ESCUREL_AUTH_GROUP_DIRECTORIES`, when a
listed directory is half-configured, or when a directory's keys are set but it is not listed.

A turn token is a chat relay, not the person on the agent surface. It may call only what a turn
needs: `whoami`; `update_page` to *create* (`base_sha256: ""`) an instance of a
`chat: {page_bound: true}` skill; `append_message` with `role: user` and `list_messages` on a
`page-chat:` group; `capture_event` under `ESCUREL_AUTH_BRIDGE_CHAT_SKILL`; `ingest_upload`.
Anything else is `-32003`, and `/ingest`, `/blob` and `/ws` answer `403`.

## Group directory

The gateway — never the bridge — looks the asker's groups up in every directory listed in
`ESCUREL_AUTH_GROUP_DIRECTORIES` and takes the **union**. Each provider maps its groups to escurel
groups by a prefix (`…_GROUP_PREFIX=escurel-`: `escurel-sales` → `sales`; other groups do not
count) and/or a JSON map file (`…_GROUP_MAP_FILE`, directory group id → escurel group). `public`,
`owner`, `admin`, `escurel:*` and the deployment's role values are never produced. Answers are
cached 10 minutes; while a directory is unreachable (429, 5xx, timeout) a cached answer stands in
for up to an hour, after which the person is told *I can't check your access right now* and no run
starts. A directory that refuses (401/403/404: a misconfiguration, an unknown user) adds nothing
for that turn and is logged — access only narrows. Every call goes through the gateway's egress
policy (https, checked and pinned addresses, no redirects, capped bodies; page links only on the
configured host).

**Microsoft Graph** (`graph`, by Entra object id). Register an app (Entra admin center → App
registrations), add the **application** permission `GroupMember.Read.All` (Microsoft Graph) and
**grant admin consent**, then create a client secret. The gateway calls
`GET /v1.0/users/{objectId}/transitiveMemberOf/microsoft.graph.group` (nested groups included).

```sh
ESCUREL_AUTH_GRAPH_TENANT_ID=<entra tenant id>
ESCUREL_AUTH_GRAPH_CLIENT_ID=<application (client) id>
ESCUREL_AUTH_GRAPH_CLIENT_SECRET_FILE=/run/secrets/graph-client-secret
ESCUREL_AUTH_GRAPH_GROUP_PREFIX=escurel-
# ESCUREL_AUTH_GRAPH_GROUP_MAP_FILE=/etc/escurel/graph-groups.json   # {"<group object id>": "sales"}
```

**Google Workspace** (`google`, by email). In a Google Cloud project enable the **Cloud Identity
API**, create a service account and a JSON key. Either grant the service account the **Groups
Reader** admin role (Admin console → Account → Admin roles), or use **domain-wide delegation** for
the scope `https://www.googleapis.com/auth/cloud-identity.groups.readonly` and name a Workspace
user to act as in `ESCUREL_AUTH_GOOGLE_SUBJECT`. The gateway calls
`GET /v1/groups/-/memberships:searchTransitiveGroups` for the sender's email.

```sh
ESCUREL_AUTH_GOOGLE_SA_KEY_FILE=/run/secrets/google-groups-sa.json
# ESCUREL_AUTH_GOOGLE_SUBJECT=groups-reader@acme.example            # domain-wide delegation
ESCUREL_AUTH_GOOGLE_GROUP_NAME=email_local                          # sales@acme.example → sales; or `email`
ESCUREL_AUTH_GOOGLE_GROUP_PREFIX=escurel-
```

**Okta** (`okta`, by email, or UPN with `ESCUREL_AUTH_OKTA_USER_KEY=upn`). Create an API token
(Security → API → Tokens) as an admin with a **read-only** role. The gateway calls
`GET /api/v1/users/{login}/groups` (paged by `Link` headers) with `Authorization: SSWS <token>`;
the group name is `profile.name`. OAuth for Okta service apps (client credentials with a private
key) is not supported yet — TODO.

```sh
ESCUREL_AUTH_OKTA_BASE_URL=https://acme.okta.com
ESCUREL_AUTH_OKTA_API_TOKEN_FILE=/run/secrets/okta-api-token
ESCUREL_AUTH_OKTA_GROUP_PREFIX=escurel-
```

A sender with no email (the members API gave none) is looked up in Graph only; the email-keyed
directories add nothing for them.

## Group chats and channels

An admin connects a shared chat to an escurel group with a page of the admin-only `chat-channel`
system skill. A group chat by its conversation id, a channel (every thread in it) by team and
channel id — the bot's refusal in an unmapped chat names the ids to use:

```sh
escurel page update markdown/instances/chat-channel/sales-chat.md <<'MD'
---
kind: instance
skill: chat-channel
id: sales-chat
channel: msteams
conversation_id: "19:…@thread.v2"      # or: team_id: "19:…@thread.tacv2" + channel_id: "19:…@thread.tacv2"
group: sales
---
# Sales group chat
MD
```

From then on a member's @mention there runs as `group:sales` with `sales`'s access only; the asker
must be a member of `sales` per the group directories, so a stranger added to the chat gets
*you're not a member* and no run. `archived: true` disconnects it; two pages naming different
groups for one chat are refused until one is removed. VS Code's Knowledge view has a *Map Teams
conversation* form for admins.

## Deprecated: the bridge's own signing key

Until this release the bridge signed each turn itself with its own RSA key
(`ESCUREL_BRIDGE_AUTH_ISSUER` / `_KID` / `_SIGNING_KEY[_FILE]` / `ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE`
on the bridge, `ESCUREL_AUTH_BRIDGE_ISSUER` / `_KID` / `_TENANT` on the gateway, the bridge serving
its JWKS at `/.well-known/jwks.json`). That path still works for this release when the bridge has
no `ESCUREL_BRIDGE_GATEWAY_TOKEN` — 1:1 chats only, and such a token carries **no groups** — and
both ends log a `DEPRECATED` warning at boot. It is removed in the next release: give the bridge an
`escurel:bridge` credential, set the gateway's template and directories, then drop the signing key
and the gateway's `ESCUREL_AUTH_BRIDGE_*` trust. The history is in
`docs/notes/discovered/2026-11-13-chat-bridge-restricted-signing-key.md`.

## Environment

The bridge's variables are the *Bridge* table of [`env.md`](env.md). A complete single-tenant
deployment (the recommended internal bot):

```sh
# escurel
ESCUREL_BRIDGE_GATEWAY_URL=http://escurel:8080
ESCUREL_BRIDGE_TENANT=acme
ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE=/run/secrets/escurel-chat-bridge-token   # roles: [escurel:bridge]
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

1. `curl -s http://escurel-chat-bridge:3978/healthz` → `ok`.
2. `escurel skill list` shows `conversation` and `chat`; `escurel runtime list` shows a runner
   `online` (minted mode).
3. In Teams, open a 1:1 chat with the bot and send *hello*. Within a few seconds the bot shows
   typing, then answers once. In a group chat, @mention the bot: before a `chat-channel` page maps
   it the bot answers once that it isn't connected, naming the conversation id.
4. If nothing comes back, read the bridge log in order: `inbound request refused` (the Bot
   Framework token: app id, endpoint URL), no `turn captured` (the gateway refused the bridge
   credential's role, `mint_user_token`'s refusal in the gateway log: template, signing key,
   directories), `turn captured` but no reply (the runner: minted mode,
   the `chat` skill), `token endpoint answered 400` (`ESCUREL_BRIDGE_TEAMS_TOKEN_URL`). The captured
   turn is a `chat` event (`list_events {label_skill: chat}` as an admin) with
   `captured_by: <the person>`, `captured_via: bridge:msteams` and the `captured_groups` the
   directories gave.

## Not verified without a real tenant

The integration test (`crates/escurel-chat-bridge/tests/teams_roundtrip.rs`) runs a real gateway,
runner and bridge against local stand-ins for Microsoft's metadata, JWKS, token endpoint,
conversation-members API and connector, and for the three group directories (Graph, Google Cloud
Identity, Okta: token endpoints, group listings, paging, error statuses). These behaviours follow
the providers' documentation and have not been checked against a real tenant:

- the `serviceurl` claim name and value on Teams channel tokens;
- the JWKS `endorsements` (channel ids per key): the bridge requires `msteams` when a key lists
  them, and accepts a key that lists none;
- Bot Framework Emulator tokens (a different issuer), which the bridge does not accept;
- inline-image downloads needing the connector bearer, and whether Teams file links redirect
  (the bridge does not follow redirects; add the final host to the allow-list if one does);
- the single-tenant token endpoint;
- Teams `groupChat` / `channel` payloads: `conversationType`, the `mention` entity naming the bot,
  `channelData.team.id` / `channel.id`, and `;messageid=<root>` in a channel thread's conversation
  id (the reply goes to that root);
- the conversation-members API returning `email` / `userPrincipalName` for a tenant user;
- Graph `transitiveMemberOf` with `GroupMember.Read.All` under admin consent, and its paging;
- Cloud Identity `searchTransitiveGroups` with the Groups Reader role or domain-wide delegation,
  the JWT-bearer token exchange, and the `discussion_forum` label filter;
- Okta `users/{login}/groups` with an SSWS token from a read-only admin, and `Link` paging.
