# Web workbench overlay contract

A downstream image can brand and extend the Escurel web workbench without patching any file of this
directory. It builds `FROM` the Escurel workbench image and adds one directory, `/opt/escurel/overlay/`.
Without that directory the image behaves exactly as the base image.

```dockerfile
FROM ghcr.io/example/escurel-workbench:1.2.3
USER root
COPY overlay/ /opt/escurel/overlay/
RUN escurel-overlay-install
USER coder
```

`escurel-overlay-install` runs at build time, as root, because the runtime root filesystem is read-only.
The entrypoint validates the same directory again at container start and refuses to start on an error
(exit 64, message `escurel-web: overlay: ...`); settings are merged at start.

## Layout

| Path | Format | Effect |
|---|---|---|
| `settings.json` | JSON object | Merged over the base machine settings (later wins). Keys are validated. |
| `workbench.css` | CSS | Appended to the workbench's global CSS (`workbench.css` of the pinned code-server), after the base CSS. |
| `login-tokens.json` | JSON object | Login/error page theme: `productName`, optional `welcomeText`, `colors` (VS Code colour id to hex). |
| `login.css` | CSS | Appended to the login page CSS. Colours only through `var(--vscode-*)`: no hex or `rgb()`/`hsl()` literals. |
| `product.json` | JSON object | Patch for code-server's `product.json`. Allowed keys: `nameShort`, `nameLong`. |
| `assets/*` | `.woff2 .woff .ttf .svg .png .ico` | Copied next to the workbench CSS and into the login pages directory (fonts, images the CSS refers to by relative URL). |
| `extensions/*.vsix` | VSIX | Installed into the image's extension directory at build. |
| `bootstrap.d/*.js` | CommonJS | Run in order of file name by the bootstrap extension on every load (see below). |

Any other entry is an error (a misspelt file name must not be silently ignored).

## Validation (build and start)

- Every JSON file parses and has the stated shape; CSS may not contain `@import` or an external `url()`.
- Size limits: JSON and CSS 256 KiB, scripts 64 KiB, assets 2 MiB, a VSIX 64 MiB. Symlinks are rejected.
- `settings.json` keys must be setting names (`a.b.c` or `[language]`). Keys that govern trust, security,
  the terminal, the extension gallery and updates, telemetry, and the values owned by the environment
  (`escurel.gatewayUrl`, `escurel.auth.*`) are refused. The workbench stays a no-terminal, no-marketplace
  surface whatever a downstream layer says.
- `login-tokens.json` colour ids must be among those the login pages use; values are `#rgb`, `#rrggbb` or `#rrggbbaa`.
  Ids you leave out keep the base theme's value.
- `bootstrap.d/*.js` pass a static lint (no `child_process`, `process.env`, network access, `eval`).
  This is a guard against accidents, not a sandbox: the boundary is that the container carries no secret
  for the extension host to read.

## Bootstrap scripts

```js
// bootstrap.d/10-landing.js
module.exports = {
  overview: false,                      // optional: do not open the Overview board on the first run
  async activate({ vscode, context, firstRun }) { /* run commands the installed extensions expose */ },
};
```

Scripts only drive the UI through the `vscode` API. They never receive a token. A script that throws or
runs longer than 10 s is logged and skipped; the others still run. If `settings.json` sets
`workbench.activityBar.location`, the bootstrap uses that value instead of hiding the activity bar.

## Paths (tests and local runs)

The defaults fit the image; tests and local runs override them with the workbench's own variables
(read by this deploy only, not by the Escurel binaries):

| Variable | Default | Used by |
|---|---|---|
| `WORKBENCH_OPT` | `/opt/escurel` | entrypoint: where `overlay.mjs` and the settings files live |
| `WORKBENCH_OVERLAY_DIR` | `$WORKBENCH_OPT/overlay` | entrypoint, bootstrap extension: the overlay directory |
| `WORKBENCH_NODE` | `/usr/lib/code-server/lib/node` | entrypoint: the Node binary that runs the overlay tools |
| `WORKBENCH_OVERLAY_SCRIPT_TIMEOUT_MS` | `10000` | bootstrap extension: time limit per `bootstrap.d` script |

## Order of effects

Settings: common < base < overlay < environment-derived values. CSS: base, then overlay. Product name:
`login-tokens.json` `productName` becomes the login title bar text and the code-server `--app-name`.

## Example

`examples/overlay-acme/` is a complete overlay for a product called `Acme Workbench` (add `extensions/*.vsix` and `assets/*` as needed), and `examples/Dockerfile.overlay-acme` is the derived image. Tests: `node --test 'deploy/web-workbench/**/*.test.mjs'`.
