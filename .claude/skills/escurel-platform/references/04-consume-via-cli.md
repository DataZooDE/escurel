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

## Subcommands (kebab-case; map 1:1 to `references/02`)

```sh
escurel list-skills
escurel list-instances --skill customer --order-by-at desc --limit 20
escurel resolve '[[customer::acme-corp]]'
escurel expand markdown/instances/customer/acme-corp.md
escurel neighbours <page_id> --direction both --link-skill meeting --limit 50
escurel search "acme churn" --k 5 --page-type instance --skill customer
escurel run-stored-query customer-churn-trend --params '{"customer_id":"acme-corp"}'
escurel update-page markdown/instances/customer/acme-corp.md   # body on stdin
```

- Every command prints a JSON object to stdout — pipe to `jq`.
  `list-skills` → `{ "skills": [ … ] }`; `resolve` →
  `{ "exists": …, "parsed": …, "page": … }`; `expand` → `{ "page", …,
  "body", "blocks", "wikilinks_out", "snapshot_version" }`; etc.
- `update-page` reads the markdown **body from stdin**:
  ```sh
  escurel update-page markdown/instances/customer/acme-corp.md < acme-corp.md
  # or:  cat acme-corp.md | escurel update-page markdown/instances/customer/acme-corp.md
  ```
- `--params` for `run-stored-query` is a JSON object string (default `{}`).
- `--page-type` is `skill` | `instance` | `any` (default `any`);
  `--direction` is `in` | `out` | `both` (default `both`);
  `limit 0` means no limit.

## Building / running it

It's the workspace's one binary, named `escurel`:

```sh
cargo build -p escurel-cli            # produces target/debug/escurel
cargo run  -p escurel-cli -- list-skills
```

The CLI needs a **running gateway** to talk to (it does not start one) —
see `references/09` for how to get one locally. It covers the eight agent
RPCs today; admin subcommands (`escurel admin …`) land with the admin
surface.

## When to prefer the CLI

- Non-Rust apps that want a stable, language-neutral entry point without
  embedding an HTTP/MCP client of their own.
- Scripted seeding/inspection in dev and CI (`escurel update-page` in a
  loop is exactly how fixtures get in — `references/07`).
- Quick interactive poking while iterating (`references/09`).

For programmatic Rust, use `escurel-client` (`references/05`) instead of
shelling out.
