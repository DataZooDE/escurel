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

Compose-level variables (`.env`): `WORKBENCH_PASSWORD`, `WORKBENCH_HASHED_PASSWORD`, `WORKBENCH_BIND`
(default `127.0.0.1`), `WORKBENCH_PORT`, `WORKBENCH_TRUSTED_ORIGINS`, `WORKBENCH_DOMAIN`,
`PROXY_HTTP_PORT`, `PROXY_HTTPS_PORT`. These belong to the image and the compose example; they are not
engine settings and are not in [`env.md`](env.md).

## The demo stack (and why its gateway is open)

| Service | Role |
|---|---|
| `workbench` | The only published service. |
| `escurel` | The gateway, **without a verifier** (no `ESCUREL_AUTH_OIDC_ISSUER`), reachable only on the compose network. The workbench's password is the access control. Everyone is the same anonymous caller, so the demo sets `ESCUREL_WRITE_ACL=off` (the per-instance write ACL has nobody to enforce against) and `ESCUREL_EGRESS_ALLOW_LOOPBACK=1` (see below). **Never publish this gateway.** |
| `runner` | The agent runner with the echo harness (a stand-in that folds a signal into a page; no model). With no verifier it runs with a static token, so changesets are not attached to the run that wrote them as they are in the desktop demo (which mints a token per run). |
| `demo-services` | A REST ratings API and an MCP confirmations server, in the **gateway's network namespace** (they listen on its loopback, which is why loopback egress is on for this demo only). |
| `demo-init`, `demo-story` | One-shot jobs: lay out the demo data under a shared volume; register the outside systems and play the story. Re-running `demo-story` on the same data duplicates the story: `docker compose down -v` first. |

The scripts are `editors/vscode/demo/web/*.sh`; the data is the desktop demo's (`editors/vscode/demo`).

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
