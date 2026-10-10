# Escurel web workbench

VS Code **in the browser** (code-server) with the Escurel extension installed, the calm
[focus layout](../../editors/vscode/docs/SPEC.md) baked in and the developer surface cut away: people who
work on orders and suppliers open one URL, log in, and land on the Overview board. Nothing is installed on
their machine.

It is the same extension as the desktop one (the VSIX built from `editors/vscode`). code-server runs the
**extension host on the server**, so the extension's Node code (HTTP and WebSocket clients, the webviews'
host side) runs unchanged; the browser only renders.

| | |
|---|---|
| Image | `deploy/web-workbench/Dockerfile` (build context = repo root) |
| Runtime | code-server **4.140.0**, pinned. It bundles VS Code 1.140.0; the extension needs >= 1.104 (`engines.vscode`). |
| One-command demo | `deploy/web-workbench/compose.yaml` (workbench + gateway + runner + demo data) |
| TLS example | `deploy/web-workbench/compose.proxy.yaml` + `Caddyfile` |
| Smoke test | `scripts/web-workbench-smoke.sh [--browser]` |

## Try it (one command)

```sh
cd deploy/web-workbench
cp .env.example .env              # set WORKBENCH_PASSWORD: the stack refuses the placeholder
docker compose up --build -d
# http://localhost:8080  (log in with the password)
```

The stack builds the gateway and runner images from this repository (a cold build takes a while: it
compiles the Rust workspace), lays out the demo data, plays the demo story, and publishes **only** the
workbench, on `127.0.0.1`. The gateway has no published port.

## Security model

This container is a **remote-code-execution surface behind one shared secret**. The design goal is that the
person who holds the password can use Escurel and nothing else. What is enforced, and where:

| Control | How | Strength |
|---|---|---|
| Password | `PASSWORD` (>= 12 chars by default, `MIN_PASSWORD_LENGTH` may lower it to 8; no placeholder) or `HASHED_PASSWORD` (argon2). The entrypoint **refuses to start** without one. code-server rate-limits login attempts. | Real gate. Prefer the hash: the secret is then not in the environment. |
| No terminal | The image **deletes `node-pty`'s native module**: the pty host cannot spawn a process. Verified in a browser: "Terminal: Create New Terminal" opens an empty tab, the server logs `No ptyHost response to createProcess`, and a typed command is never executed. The settings (`terminal.integrated.profiles` = `/bin/false`, terminal keybindings unbound) are defence in depth only; the keybinding removals were not tested. | Hard. Does not depend on a setting a person can edit. Re-applied (and asserted by the smoke test) on every code-server bump. |
| No process-spawning extensions | git, GitHub/Microsoft sign-in, Copilot, the JS debugger, task providers (npm, grunt, gulp, jake) removed from the image. | Hard (files are gone). |
| No marketplace | `EXTENSIONS_GALLERY={}`; the extensions directory is in the image and root-owned, so installing a local VSIX should fail on permissions (not attempted: a VSIX cannot be authored in the editor and uploads are off). Verified in a browser: the Extensions view offers nothing installable. | Hard. |
| No uploads / downloads / proxy | `--disable-file-uploads --disable-file-downloads --disable-proxy` (no `/proxy/<port>` into the container). | Hard. |
| Non-root, no sudo, no setuid | uid 1000; `sudo` and `fixuid` removed, setuid bits stripped. The compose file adds `read_only: true`, `cap_drop: [ALL]`, `no-new-privileges`, tmpfs for scratch, memory and pid limits, and no docker socket. | Hard. |
| No credential in the image or the environment | The extension takes a bearer only from its own sign-in (OIDC), never from a setting or an environment variable (a design rule of the extension). The bootstrap extension in the image runs two view commands and carries no secret. | By design. |
| Calm layout (no menu bar, status bar, activity bar, stock Explorer/Search/SCM/Run/Extensions) | Baked user settings. | **Cosmetic**, not a boundary: the command palette still lists generic VS Code commands, and a person can edit their own settings. |

### What is NOT blocked (read this before exposing it)

- **The server's filesystem is readable by the logged-in user** (File > Open File shows the container's
  directories as the `coder` user; `/proc/self/environ` includes `PASSWORD` if you used a plain password).
  This is inherent to VS Code. The container holds no secret besides the password, which the user already
  knows, which is one more reason to use `HASHED_PASSWORD`.
- **One shared password = one identity.** Everyone who knows it is the same person to code-server. Per-user
  identity belongs to the *gateway's* sign-in (OIDC), not to this container. Run one container per person or
  team if their gateway permissions differ.
- **Files in the workspace are writable** (tmpfs in compose, gone on restart): a person can create markdown
  there. It cannot be run: there is no terminal and no task runner.
- **The gateway is reachable from the container** (that is its job). With a verifying gateway (OIDC) the
  session token sits in code-server's secret storage under the data volume, readable by the same user.
- **Webviews need a trusted origin.** Service workers refuse to register on an *untrusted* certificate and
  on plain `http://` to anything but `localhost`/`127.0.0.1`. Use a real certificate (a public domain, or
  your own CA trusted by the browser) for anything off the local machine. With Caddy's local CA
  (`WORKBENCH_DOMAIN=localhost`) every webview (Overview, thread canvas, run detail) is blank with
  "Error loading webview" until the browser trusts that CA. Plain `http://localhost:8080` works.

If that is not acceptable, do not expose the workbench: keep `WORKBENCH_BIND=127.0.0.1` and reach it over an
SSH tunnel or a VPN.

## The login page

The sign-in page is code-server's own, re-skinned to look like the Escurel Calm window that opens after login: the
page uses the same `--vscode-*` colour names as the extension's webviews (editor surface, a title bar with the
Escurel glyph, a centred workbench widget with a 1px border, 13px system UI font, 2px control radius, the Calm teal
button and focus ring). It is always the light Calm look, whatever the browser prefers, because the workbench behind
it applies the Calm theme. The colours are not copied by hand: `deploy/web-workbench/login/gen-global-css.mjs`
writes `login/global.css` from `editors/vscode/themes/escurel-calm-color-theme.json`; `node --test
deploy/web-workbench/login/login-tokens.test.mjs` (also run by the smoke script) and an image build stage fail when
the committed file drifts from the theme or a text/button colour drops below WCAG AA. After editing the theme, run
`node deploy/web-workbench/login/gen-global-css.mjs`. The files live in `deploy/web-workbench/login/` and the
Dockerfile installs them over code-server's `login.html`, `login.css`, `global.css`, `error.css` and its icons; the
build fails if a pinned code-server version moves those files or if the template loses `{{ERROR}}` or the
`password` field. The form posts exactly as before, so login, the error message ("Incorrect password"), rate
limiting and the `base`/`href` path handling are unchanged. The page title, welcome text (`WORKBENCH_WELCOME_TEXT`,
default "Sign in to Escurel") and favicon say Escurel. The stock "check ~/.config/code-server/config.yaml for the
password" hint is dropped from the page. The pages' CSP is `style-src 'self'`, so the skin is plain CSS files:
no inline styles, no external fonts.

Not overridable: the text of code-server's error strings (they come from its locale files; only the page chrome is
ours), `--welcome-text` is deprecated upstream (it still works on 4.140.0; the login template could hard-code it
instead), and the workbench itself takes its theme from the baked settings, not from this page. The unfocused input border is the
theme's own `input.border` (about 1.7:1 on the widget): the field is autofocused and shows the 5.8:1 focus ring.

## TLS

Put a TLS-terminating reverse proxy in front. `compose.proxy.yaml` is a minimal Caddy example with
automatic certificates:

```sh
# .env: WORKBENCH_DOMAIN=workbench.example.com
docker compose -f compose.yaml -f compose.proxy.yaml up --build -d
```

It removes the workbench's published port; only Caddy listens on the host. WebSockets pass through
unchanged. Serve the workbench on its **own host name**: serving it under a path prefix was not tested.
If the browser's origin differs from what the proxy forwards, set `WORKBENCH_TRUSTED_ORIGINS`
(compose) / `CODE_SERVER_TRUSTED_ORIGINS` (container). Alternatively give the container `--cert` and
`--cert-key` directly (`CODE_SERVER_CERT`, `CODE_SERVER_CERT_KEY`).

## Your own gateway

Drop everything but `workbench` from the compose file and set:

| Variable (container) | Meaning |
|---|---|
| `PASSWORD` / `HASHED_PASSWORD` | The workbench password (see above). One of them is required. |
| `WORKBENCH_GATEWAY_URL` | **Required.** The gateway, e.g. `https://escurel.example.com`. Written to the extension's `escurel.gatewayUrl`. |
| `WORKBENCH_AUTH_ISSUER`, `WORKBENCH_AUTH_CLIENT_ID` | For a gateway that verifies tokens: the OIDC issuer and client id of the extension's sign-in (`escurel.auth.issuer`, `escurel.auth.clientId`). Unset = the gateway is used without a token (only for a gateway that runs without a verifier). The sign-in redirect goes through VS Code's `asExternalUri`; **not exercised in this image** (see known gaps). |
| `PORT` | Listen port (default 8080). |
| `WORKBENCH_DATA_DIR` | User data (settings, browser-session state, the extension's secret storage). Default `/home/coder/data`; mount a volume to keep it. |
| `CODE_SERVER_CERT`, `CODE_SERVER_CERT_KEY` | Serve HTTPS directly (both or neither). |
| `CODE_SERVER_TRUSTED_ORIGINS` | Extra origins accepted behind a proxy. |
| `CODE_SERVER_ABS_PROXY_BASE_PATH` | Passed through; path-prefix serving is untested. |

Compose-level and script variables (`.env` next to `compose.yaml`, or the shell). They belong to the
image, the compose example and the helper scripts; they are not engine settings and are not in
[`env.md`](env.md).

| Variable | Default | What it does |
| --- | --- | --- |
| `WORKBENCH_PASSWORD` / `WORKBENCH_HASHED_PASSWORD` | — (required, one of them) | The one shared password (plain, or an argon2 hash: preferred, the secret then never sits in the environment). |
| `WORKBENCH_MIN_PASSWORD_LENGTH` | `12` | Shortest plain password accepted; may be lowered to `8`, never below. Lowering it weakens the only gate. |
| `WORKBENCH_BIND` / `WORKBENCH_PORT` | `127.0.0.1` / `8080` | Where the workbench port is published. `0.0.0.0` is every interface: only behind TLS. |
| `WORKBENCH_TRUSTED_ORIGINS` | — | The browser's origin when a reverse proxy sits on another origin. |
| `WORKBENCH_DOMAIN`, `PROXY_HTTP_PORT`, `PROXY_HTTPS_PORT` | — / `80` / `443` | The Caddy TLS front of `compose.proxy.yaml`. |
| `WORKBENCH_WELCOME_TEXT` | `Sign in to Escurel` | The login page's heading. |
| `WORKBENCH_GATEWAY_URL`, `WORKBENCH_AUTH_ISSUER`, `WORKBENCH_AUTH_CLIENT_ID`, `WORKBENCH_DATA_DIR` | see "Your own gateway" | Image-level settings written into the extension's configuration. |
| `WORKBENCH_COMPOSE_PROJECT` | `escurel-web` | The compose project `s2d-compose.sh` operates on. |
| `WORKBENCH_PROJECT` | the compose default | The project `s2d-up.sh` creates (a throwaway instance: `WORKBENCH_PROJECT=escurel-web-test WORKBENCH_PORT=18090`). |
| `WORKBENCH_S2D_DIR` | `~/.cache/escurel-web-s2d` | Where `s2d-up.sh` puts the synced S2D data the gateway container mounts. |
| `S2D_HETZNER_REPO` | `~/Projects/datazoo/hetzner-agent-substrate` | The checkout the S2D sync reads the seed from (read-only). |
| `S2D_CLI_BIN` | `target/release/escurel` | The `escurel` CLI for `migrate-kind-files` on the synced seed. |
| `S2D_UP_TIMEOUT_SECS` | `1500` (`s2d-up.sh`) / `600` (`s2d-reset.sh`) | How long the scripts wait for the story to be played. |
| `S2D_DIR`, `S2D_INDEX_EXT`, `S2D_EXT_DIR`, `S2D_ALLOW_UNSIGNED`, `S2D_GATEWAY_DUCKDB` | recorded by `s2d-up.sh` | Hand-offs from `s2d-up.sh` to `compose.s2d.yaml` (the data dir, the optimizer extension and its unsigned-load switch); not meant to be set by hand. |
| `WORKBENCH_IMAGE`, `WORKBENCH_SMOKE_PORT`, `WORKBENCH_SMOKE_OUT` | `escurel-web-smoke/workbench:dev` / a free port / `/tmp/escurel-web-smoke` | `scripts/web-workbench-smoke.sh`: the image it builds and probes, the host port, where the browser probe writes its screenshots. |

The desktop demo launcher's own knobs are listed in `editors/vscode/demo/README.md` (its variable names carry the
demo prefix and are deliberately not named here: this file is scanned for engine settings).

## The demo stack (and why its gateway is open)

| Service | Role |
|---|---|
| `workbench` | The only published service. |
| `escurel` | The gateway, **without a verifier** (no `ESCUREL_AUTH_OIDC_ISSUER`), reachable only on the compose network. The workbench's password is the access control. Everyone is the same anonymous caller, so the demo sets `ESCUREL_WRITE_ACL=off` (the per-instance write ACL has nobody to enforce against) and `ESCUREL_EGRESS_ALLOW_LOOPBACK=1` (see below). **Never publish this gateway.** |
| `runner` | The agent runner with the echo harness (a stand-in that folds a signal into a page; no model). With no verifier it runs with a static token, so changesets are not attached to the run that wrote them as they are in the desktop demo (which mints a token per run). |
| `demo-services` | A REST ratings API and an MCP confirmations server, in the **gateway's network namespace** (they listen on its loopback, which is why loopback egress is on for this demo only). |
| `demo-init`, `demo-story` | One-shot jobs: lay out the demo data under a shared volume; register the outside systems and play the story. Re-running `demo-story` on the same data duplicates the story: `docker compose down -v` first. |

The scripts are `editors/vscode/demo/web/*.sh`; the data is the desktop demo's (`editors/vscode/demo`).

## The Source-to-Deliver (S2D) demo stack

For the Bosch live part the workbench carries the S2D demo (three agent proposals waiting for a planner, the brain-teasers):
data, skills and reports come from the `hetzner-agent-substrate` seed (the single source), built on the host, nothing of it is committed
here. It adds `compose.s2d.yaml` to the base stack:

```sh
cd deploy/web-workbench
./s2d-up.sh        # builds, (re)seeds, plays the story; the .env next to compose.yaml holds WORKBENCH_PASSWORD
./s2d-reset.sh     # back to the STARTING STATE: the three proposals open in "Awaiting you", nothing promoted
./s2d-compose.sh ps | logs -f demo-services | down     # docker compose with the variables s2d-up.sh recorded
```

What is different from the generic demo stack, and why:

* **The gateway verifies tokens** (`escurel-test-gateway`, the `demo-gateway` target of the Dockerfile: escurel-server plus a built-in issuer),
  because the demo shows which run wrote what (the thread, the run trace, the proposals held for a person). It is reachable only on the compose
  network. The **runner mints a token per run**. The optimizer extension (anofox_optimize) is loaded when a build for the gateway's DuckDB
  version exists on the host (otherwise those two query pages are left out, with a warning from the sync).
* **The workbench holds no credential.** A small reverse proxy (`editors/vscode/demo/web/forward.mjs`, in `demo-services`) is `escurel:8080`: it
  signs every call in as the demo user. The extension runs in its plain "no token" mode; no token, admin bearer or signing key enters the
  workbench container.
* **A new gateway starts empty.** Its data lives in its container, so `demo-services` plays the story (and loads the S2D demo) whenever the
  gateway is new, and the runner's ledger is wiped at its start. A `docker compose restart`, `./s2d-reset.sh` or a reboot therefore brings the demo
  back in its STARTING STATE (everything promoted on stage is gone: that is the reset). After a reboot, if the demo is not back within two
  minutes, run `./s2d-reset.sh` (or `./s2d-up.sh`): the runner and the services share the gateway's network namespace, so they must start after it.
* `./s2d-up.sh` rebuilds the gateway-side volumes from scratch (a replay on old data would duplicate the story); the workbench's own volume
  (VS Code settings) is kept.

The click path (stories, brain-teasers, expected numbers) is `editors/vscode/demo/s2d/REHEARSAL.md`;
`deploy/web-workbench/probe/s2d-tour.mjs <url> <password-file> <out-dir>` walks it in a headless browser and takes a screenshot per step
(it approves the proposals: run it on a throwaway instance, e.g. `WORKBENCH_PROJECT=escurel-web-test WORKBENCH_PORT=18090 ./s2d-up.sh`).
Rollback to the generic stack: check out the previous commit and run `docker compose up --build -d` (the S2D volumes can be removed with
`docker compose down -v`, which also removes the workbench's own data).

## Operating it

- **Upgrade**: change the pinned `codercom/code-server` tag in the Dockerfile, check that its bundled VS
  Code is still >= `engines.vscode`, rebuild, run `scripts/web-workbench-smoke.sh --browser`. The `node-pty`
  removal and the deleted extension directories are patches of the upstream image: the smoke test
  asserts them, so a layout change upstream fails loudly instead of silently re-enabling a terminal.
- **State**: the browser keeps its own layout (which views are open); the data volume keeps settings and
  secret storage. Losing the volume signs the extension out; nothing else is lost.
- **Health**: `GET /healthz` (unauthenticated, dependency-free) is the container's `HEALTHCHECK`.
- **Logs**: code-server logs to stdout. The `File not found: .../github-authentication/dist/browser/...`
  lines on page load are the browser asking for built-in extensions that the image removes; harmless.
- **Size**: about 1.05 GB (code-server's own image is most of it).

## Known gaps

- OIDC sign-in from the web workbench (PKCE through `asExternalUri`) is wired through settings but **was not
  exercised** against a real issuer here; the demo uses a gateway without a verifier.
- Serving under a URL path prefix is untested.
- Single shared password; no per-user identity or audit at the workbench layer.
- Removing `node-pty` is a patch on the upstream image, not a supported VS Code option.
- No CI job builds this image yet; run the smoke script after changing `deploy/web-workbench/` or the
  extension.
