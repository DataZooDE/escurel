# 12 — escurel as the memory MCP behind an external agent

Coordination tools (Multica, an issue board, a chat bot) and coding agents
(Claude Code, Codex) keep an agent's memory as issue history plus flat
`SKILL.md` playbooks. escurel can sit underneath them as the typed,
provenance-tracked memory: the agent keeps running where it runs, and reads
and writes knowledge through escurel's `/mcp`. Nothing new runs inside
escurel for this — it is the ordinary MCP-over-HTTP surface (`references/03`).

## What the agent connects to

| | |
|---|---|
| URL | `https://<gateway>/mcp` (`http://127.0.0.1:8080/mcp` locally) — streamable-HTTP MCP, JSON responses, no SSE stream |
| Header | `Authorization: Bearer <token>` on every call |
| Tenant | **no header**: one gateway serves one tenant, named by the token's tenant claim (`tenant`, `escurel_tenant` on the substrate; `references/08`). A token for another tenant is refused |
| Discovery | `tools/list` is role-scoped: the agent sees only what its token may call |

Check the wiring before handing it to an agent:

```sh
ESCUREL_SERVER=https://<gateway> ESCUREL_TOKEN=<token> escurel whoami   # {subject: "agent:scribe", tenant, is_admin: false}
```

## The token: what exists today

Pick by who the agent acts for. Never give an external agent an admin token.

1. **An agent profile's token (the default for an external agent, #829).**
   An admin writes an agent page (`references/01`, *Agent profiles*: the
   `skills` it may run, `scopes.tools`, `scopes.write`) and issues a token
   that runs as it:

   ```sh
   escurel agent token issue scribe --ttl 30d   # admin; prints {token, subject, expires_at} ONCE
   ```

   (`issue_agent_token`, `references/02`). The token is `sub: agent:scribe`,
   `act.sub: <the issuing admin>`, `escurel:agent` plus the write groups of
   the skills the agent lists, and the `agent` claim — no run, no skill, never
   admin. The gateway reads the agent page on **every** call: `tools/list`
   offers only `scopes.tools`, a write outside `scopes.write` is
   `agent_scope_denied`, and `/ws`, `/ingest` and `/blob` refuse it. Life 1 h
   to 90 d (default 30 d). **Revoke** every token issued so far with
   `escurel agent token revoke scribe` (it sets the page's `token_epoch` to
   the next second): the next call with an older token is
   `agent_token_revoked`, while a token issued afterwards works. Needs the
   gateway's signing key (`ESCUREL_AUTH_SIGNING_KEY`/`_KID`); without one the
   tool answers `unsupported`. Store the token where the harness reads it (an
   env var or a secret), never in git; escurel does not keep a copy.
2. **A person's agent (an interactive Claude Code / Codex session).** The
   person mints a short-lived bearer *for an agent acting for them*:
   `escurel run mint-token --skill <skill> [--ttl-secs N]` (`mint_agent_token`,
   `references/02`). It is `sub: agent:<skill>`, `act.sub: <the person>`,
   their own groups, **never admin**, `ttl_secs` default 1800, max 14400,
   **no renewal** — mint a new one when it lapses. Whatever the agent writes
   reads `captured_by: <person>` / `captured_via: agent:<skill>`, and its
   drafts and events carry the minted `run_id`, so the session shows up in
   `list_events{run_id}` and the run transcript. Only a person's token can
   mint — an agent token cannot mint another.
3. **A service credential from your IdP**, restricted by a role allow-list
   (`ESCUREL_AUTH_ROLE_TOOLS`, `references/08` *Service roles*), when the
   deployment already issues one per agent. Revocation is then by expiry
   only; prefer option 1.

## Wiring it up

**Claude Code** — one command (local scope; `-s project` writes `.mcp.json`):

```sh
claude mcp add --transport http escurel https://<gateway>/mcp \
  --header "Authorization: Bearer $ESCUREL_TOKEN"
```

or check it in as `.mcp.json`, reading the token from the environment so it
never lands in git:

```json
{
  "mcpServers": {
    "escurel": {
      "type": "http",
      "url": "https://<gateway>/mcp",
      "headers": { "Authorization": "Bearer ${ESCUREL_TOKEN}" }
    }
  }
}
```

**Codex** — `codex mcp add escurel --url https://<gateway>/mcp
--bearer-token-env-var ESCUREL_TOKEN`, or in `~/.codex/config.toml`:

```toml
[mcp_servers.escurel]
url = "https://<gateway>/mcp"
bearer_token_env_var = "ESCUREL_TOKEN"
```

**Multica** — each Multica agent runs on a coding-agent CLI (Claude Code,
Codex) and carries its own MCP server list: add escurel there with the same
URL and `Authorization` header as above, one issued agent token per Multica
agent (option 1, one agent page each), so the board's agents do not share an
identity and each can be revoked on its own. Check Multica's agent
settings for the exact field names; escurel needs nothing beyond URL + bearer.

## Which tools to use for memory

| need | tool | note |
|---|---|---|
| recall | `search` (`q`, `k`, `page_kind`, `skill`) | hybrid vector + full text; start here |
| follow a link | `resolve` (`wikilink`) → `expand` (`page_id`) | `[[skill::id]]` from a page or a SKILL.md |
| context around a page | `neighbours` | typed links in and out |
| what kinds of memory exist | `list_skills` | the tenant's types and their fields |
| record what happened | `capture_event` (`label_skill`, `title`, `body`, `provenance`) | the default write: an event is an append-only fact, and the skill's runner or a curator turns it into pages |
| change a page | `update_page` (`page_id`, `content`, `base_sha256`) | only where the skill's `autonomy` and `acl` allow the agent to write; read `content_sha256` from `expand` first so a concurrent edit conflicts instead of being overwritten |
| propose a change | `create_draft` | for `autonomy: review` skills: a person promotes it |

Prefer `capture_event` over `update_page` for an agent that is not the owner
of the knowledge: it keeps provenance (`captured_by` / `captured_via`), never
overwrites, and leaves the shaping of pages to the skill that owns them.

## Giving the agent the playbooks too

The agent's `SKILL.md` playbooks can come *from* escurel, so the instructions
and the memory stay one source:

```sh
escurel skill export triage customer --out .claude/skills   # .claude/skills/<skill>/SKILL.md
escurel skill import ./some-skill                            # back in, as a draft for review
```

The exported body keeps its `[[skill::id]]` links readable and appends an
"escurel references" section telling the agent to look each one up with
`resolve` — which is the MCP server configured above. escurel-only keys ride
in `metadata.escurel` (`references/04`, *Notes on shape*). Anthropic's
`name` is lowercase letters, digits and hyphens (at most 64): a skill id with
`_` or `.` exports as it is and some agents will refuse it, so rename such a
skill before you rely on the export.
