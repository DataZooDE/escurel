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
ESCUREL_SERVER=https://<gateway> ESCUREL_TOKEN=<token> escurel whoami   # {subject, tenant, is_admin}
```

## The token: what exists today

Pick by who the agent acts for. Never give an external agent an admin token.

1. **A person's agent (interactive Claude Code / Codex session).** The
   person mints a short-lived bearer *for an agent acting for them*:
   `escurel run mint-token --skill <skill> [--ttl-secs N]` (`mint_agent_token`,
   `references/02`). It is `sub: agent:<skill>`, `act.sub: <the person>`,
   their own groups, **never admin**, `ttl_secs` default 1800, max 14400,
   **no renewal** — mint a new one when it lapses. Whatever the agent writes
   reads `captured_by: <person>` / `captured_via: agent:<skill>`, and its
   drafts and events carry the minted `run_id`, so the session shows up in
   `list_events{run_id}` and the run transcript. Needs the gateway's signing
   key (`ESCUREL_AUTH_SIGNING_KEY`/`_KID`); without one the tool answers
   `unsupported`. Only a person's token can mint — an agent token cannot mint
   another.
2. **An unattended board agent (a Multica agent working tickets).** A service
   credential from the deployment's OIDC issuer (client credentials), with
   `aud: escurel`, the tenant claim, and a **service role** in its groups that
   `ESCUREL_AUTH_ROLE_TOOLS` restricts to a tool list, e.g.

   ```sh
   ESCUREL_AUTH_ROLE_TOOLS="app:board=search,resolve,expand,neighbours,list_skills,list_instances,capture_event,create_draft"
   ```

   The skills then say what that role may read and propose (`acl.read_events`,
   `acl.read_referenced`, `acl.propose`; `references/01`). `create_draft`
   needs an `acl.propose` grant; `capture_event` from such a role must reply
   to an event it can read. Revocation is by expiry: the gateway verifies
   self-contained JWTs, so keep the access-token lifetime short
   (`references/08`, *Service roles*).

**Agent profiles (#823) are not an external-agent credential.** An agent
profile (`references/01`, *Agent profiles*: `harness`, `model`, `scopes.tools`,
`scopes.write`) binds tokens that **escurel-runner** mints for runs it
executes; the gateway does not mint profile tokens for outside callers, and
`mint_agent_token` takes no agent. To have work done under a profile's
least-privilege scopes, let the external agent *ask* rather than *do*: it
files the request with `capture_event` under a skill whose `agent:` names the
profile, and the runner executes it (`references/11`). For direct calls,
option 2's role allow-list is today's equivalent of `scopes.tools`.

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
URL and `Authorization` header as above, one credential per agent (option 2),
so the board's agents do not share an identity. Check Multica's agent
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
