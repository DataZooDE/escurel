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

The extension runs in Restricted Mode windows too; there its `escurel.*` settings are read from your user settings only (a folder cannot redirect you to another gateway).

Press F5 in VS Code (`editors/vscode` as the workspace) for the Extension
Development Host; point `escurel.gatewayUrl` at a running `escurel-server`.
