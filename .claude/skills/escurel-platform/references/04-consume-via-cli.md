# 04 — Consume via the `escurel` CLI

The `escurel` binary (`crates/escurel-cli`) is a **thin MCP-over-HTTP client** for
the agent surface — one subcommand per read/write tool, JSON on stdout.
Ideal for shells, scripts, Makefiles/justfiles, CI smoke checks, and
non-Rust apps that prefer to shell out rather than embed a client.

## Connecting

```sh
export ESCUREL_SERVER="http://127.0.0.1:8080"   # HTTP MCP endpoint; this is the default
export ESCUREL_TOKEN="<bearer>"                 # omit only if the server runs unauthenticated
```

Both also have flags: `--server <url>` and `--token <jwt>`. The token is
hidden from `--help` env dumps. With no token the CLI sends RPCs without
an `authorization` header and lets the server enforce its own policy
(dev/on-host mode). Auth details: `references/08`.

`--format json` (default) emits the stable JSON contract on stdout;
`--format table` renders a human table. It's global — put it anywhere.
Errors go to **stderr** as JSON with a non-zero exit, so a calling agent
can branch on them.

## Command shape

Commands are grouped **gh/aws-style by resource noun** (`escurel <noun>
<verb>`), plus two bare top-level verbs (`search`, `resolve`). Each maps
1:1 to a tool in `references/02`:

```sh
# search + resolve (top-level verbs)
escurel search "acme churn" --k 5 --page-type instance --skill customer
escurel resolve '[[customer::acme-corp]]'

# skills + instances
escurel skill list
escurel instance list --skill customer --order-by-at desc --limit 20

# pages
escurel page expand    markdown/instances/customer/acme-corp.md
escurel page validate  markdown/instances/customer/acme-corp.md   # body on stdin, commits nothing
escurel page update    markdown/instances/customer/acme-corp.md   # body on stdin, upserts
escurel page blob      <page_id>                                  # original bytes of a document instance
escurel page snapshots <page_id>                                  # CRDT snapshot timestamps (time-travel cuts)

# link graph
escurel link neighbours <page_id> --direction both --link-skill meeting --limit 50

# stored queries + query pages
escurel query run      customer-churn-trend --params '{"customer_id":"acme-corp"}'
escurel query instance '[[query::pipeline-by-stage]]' --params '{"from":"2026-01-01"}'

# the event bus (M7)
escurel event capture --source gmail --mime message/rfc822 --label-skill meeting \
                      --title "Kickoff" --instance <page_id>       # body on stdin unless --body
escurel event inbox  --limit 50
escurel event list   --instance <page_id> --limit 50
escurel event assign --event <event_id> --instance <page_id>

# per-chat-group conversation history
escurel chat append -g <chat_group_id> --role user                 # content on stdin unless --content
escurel chat list   -g <chat_group_id> --since <rfc3339> --limit 100 --direction desc

# live CRDT co-editing session (raw op passthrough)
escurel session open  <page_id>                                    # → {session, head_version, ws_url}
escurel session apply <session> --op <base64>                      # op on stdin unless --op
escurel session close <session> [--no-commit]                      # snapshots the doc unless --no-commit

# document ingestion (POST /ingest/upload, not an MCP tool)
escurel ingest ./memo.txt --content-type text/plain                # skill resolved from the MIME
escurel ingest ./deck.pdf --content-type application/pdf --title "Q3 deck" --skill fraktion-a

# operator surface (admin-role token; `health` needs none)
escurel admin health
escurel admin tenant create --id acme --name "Acme Corp"
escurel admin quota  --tenant acme
escurel admin rebuild --tenant acme
```

### CLI command → tool map

| CLI | Tool (`references/02`) |
|---|---|
| `search` / `resolve` | `search` / `resolve` |
| `skill list` / `instance list` | `list_skills` / `list_instances` |
| `page expand` / `page validate` / `page update` | `expand` / `validate` / `update_page` |
| `page blob` / `page snapshots` | `fetch_blob` / `list_snapshots` |
| `link neighbours` | `neighbours` |
| `query run` / `query instance` | `run_stored_query` (legacy) / `query_instance` |
| `event capture\|inbox\|list\|assign` | `capture_event` / `list_inbox` / `list_events` / `assign_event` |
| `chat append` / `chat list` | `append_message` / `list_messages` |
| `session open\|apply\|close` | `open_session` / `apply_op` / `close_session` |
| `ingest` | `POST /ingest/upload` (HTTP endpoint) |
| `admin …` | the EscurelAdmin surface |

The mapping is enforced by a **parity guard test**
(`crates/escurel-cli/tests/cli_parity.rs`): every agent-role tool the
gateway advertises in `tools/list` must have a CLI command, so this table
can't silently drift as new tools land. The admin/ops *provisioning*
MCP-twins (credential/endpoint/group management, `create_sql_instance`,
`write_instance`, lane/index introspection) are deliberately CLI-less —
drive them over MCP/gRPC or the BFF.

## Notes on shape + switches

- Every command prints a JSON object to stdout — pipe to `jq`.
  `skill list` → `{ "skills": [ … ] }`; `resolve` →
  `{ "exists": …, "parsed": …, "page": … }`; `page expand` →
  `{ "page", …, "body", "blocks", "wikilinks_out", "snapshot_version" }`;
  `page blob` → `{ "blob": { "page_id", "content_type", "size",
  "bytes_base64" } }` (null blob for a non-document page).
- **stdin-body** commands: `page validate`, `page update` (markdown body),
  `event capture` (event body, unless `--body`), `chat append` (content,
  unless `--content`), `session apply` (base64 op, unless `--op`):
  ```sh
  escurel page update markdown/instances/customer/acme-corp.md < acme-corp.md
  ```
- `--params` for `query run` / `query instance` is a JSON object string
  (default `{}`).
- `--page-type` is `skill` | `instance` | `any` (default `any`);
  `--direction` is `in` | `out` | `both` (default `both`); `limit 0`
  means no limit.
- `ingest --skill <id>` pins a specific `document`-backend skill and
  triggers that skill's **create-ACL** (a 403 if the caller can't create
  there); omit it to resolve the handling skill from the MIME.
- `session` commands need the gateway booted with a CRDT backend, else
  they return a JSON-RPC error explaining live mode is disabled. The raw
  base64 op is the wire contract — a browser/editor peer is the real
  driver; the CLI is for scripted open/apply/close.

## Building / running it

It's the workspace's one binary, named `escurel`:

```sh
cargo build -p escurel-cli            # produces target/debug/escurel
cargo run  -p escurel-cli -- skill list
```

The CLI needs a **running gateway** to talk to (it does not start one) —
see `references/09` for how to get one locally. It also ships `escurel ui`,
an interactive k9s-style terminal browser over the same agent surface.

## When to prefer the CLI

- Non-Rust apps that want a stable, language-neutral entry point without
  embedding an HTTP/MCP client of their own.
- Scripted seeding/inspection in dev and CI (`escurel page update` in a
  loop is exactly how fixtures get in — `references/07`).
- Quick interactive poking while iterating (`references/09`).

For programmatic Rust, use `escurel-client` (`references/05`) instead of
shelling out.
