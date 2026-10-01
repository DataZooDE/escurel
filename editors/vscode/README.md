# escurel — VS Code extension

Skills, instances, events and runs of one escurel gateway, inside VS Code.
The binding spec is `docs/SPEC.md`; backend prerequisites and their
degradations are tracked in `docs/BACKEND_GAPS.md`.

```sh
npm ci
npm run build          # dist/extension.js + dist/webview/*.js
npm test               # typecheck + lint (incl. no literal colours in webview/) + unit + component
npm run test:visual    # Playwright screenshots in light / dark / high-contrast, inside the pinned Playwright image (docker)
npm run test:visual:update   # regenerate the baselines the same way
npm run package        # escurel-<version>.vsix
```

Integration tests drive a real VS Code against real binaries; build them first and the harness finds them in `target/release/`:

```sh
cargo build --release -p escurel-server -p escurel-runner -p escurel-test-support --bin escurel-server --bin escurel-runner --bin escurel-test-gateway
npm run test:integration
```

Two passes run: the corpus suite against `escurel-server` (no verifier, no runner), and the cascade suite against `escurel-test-gateway` (a gateway that verifies tokens, so a minted-mode runner can prove which run wrote what) plus a real runner. A missing binary skips the cascade pass with a warning. Override the paths with `ESCUREL_SERVER_BIN`, `ESCUREL_RUNNER_BIN` and `ESCUREL_TEST_GATEWAY_BIN`.

The extension runs in Restricted Mode windows too; there its `escurel.*` settings are read from your user settings only (a folder cannot redirect you to another gateway).

Press F5 in VS Code (`editors/vscode` as the workspace) for the Extension
Development Host; point `escurel.gatewayUrl` at a running `escurel-server`.
