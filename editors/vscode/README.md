# escurel — VS Code extension

Skills, instances, events and runs of one escurel gateway, inside VS Code.
The binding spec is `docs/SPEC.md`; backend prerequisites and their
degradations are tracked in `docs/BACKEND_GAPS.md`.

## What you can do

- **Browse and read** skills and their instances (Knowledge), search, follow wikilinks; an instance opens as a
  form (typed fields, the page body with its tables) or as Markdown.
- **Review**: the Inbox, *Awaiting you*, a diff with comments, promote or discard a draft or a whole changeset.
- **Follow work as it happens**: the thread of an event (event → run → changeset → follow-on events) and each
  run's plan, attempts and tool calls, live, without reload.
- **Start a skill** from an instance or a thread node: in the background, *first make a plan* and approve it,
  or in a terminal (a token minted for that one run; the run still shows up in the thread as a governed run).
- **Run the runner**: the Runner view (secondary sidebar) shows its health, live runs and dead letters; cancel
  or retry a run; an admin can also requeue a dead letter and pause or resume dispatch. A human sees those
  admin controls deactivated, with the reason, not hidden.

## Security model

The gateway decides what you may do; the extension only avoids offering what it knows will be refused. What it
does itself:

- Settings that decide where your token goes or what is executed (`escurel.gatewayUrl`, `escurel.auth.*`,
  `escurel.harness`, `escurel.shellHarness`) are **user-level only**: a repository's `.vscode/settings.json`
  cannot set them (`docs/notes/discovered/2026-10-02-vscode-workspace-must-not-choose-the-gateway.md`).
- Webviews hold no token and are not trusted: every message they send is checked against what the extension
  host itself loaded before anything is written.
- Starting in a terminal runs your own `escurel.shellHarness` command, only in a trusted workspace, and only
  when you click it.
- A production install exports no API to other extensions; the test and demo harnesses get one in Development
  and Test mode only.

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

The corpus suite uses `escurel-server` without a runner; the cascade and controls suites use `escurel-test-gateway` with a verifying issuer and a real runner. A missing binary skips the cascade pass for local exploratory runs. Set `ESCUREL_REQUIRE_CASCADE=1` to make missing binaries or a missing runner fail; the VS Code CI job sets this for the Evolve plan review suite. Override binary paths with `ESCUREL_SERVER_BIN`, `ESCUREL_RUNNER_BIN` and `ESCUREL_TEST_GATEWAY_BIN`.

Live end-to-end tests drive a real VS Code window by clicking: `npm run test:e2e`. They bring up their
own stack (a gateway that verifies tokens, a runner whose echo harness idles about 6 s so a run is still
live long enough to be clicked, the story from `demo/`) on a virtual display (Xvfb), attach Playwright over
CDP, and leave a screenshot per step in `test/e2e/artifacts/` to LOOK at: an assertion passing says
nothing about whether it looks right. They need the built extension, the release binaries
(`ESCUREL_BIN_DIR`, default `<repo>/target/release`) and Xvfb. Chromium's site isolation is turned off
for them, since a VS Code webview is an out-of-process iframe Playwright cannot otherwise see into; they
never touch a window you have open (their own display and profile).

The minimum VS Code is **1.104**: the Runner view lives in the secondary sidebar, which an extension can contribute to from that release (`docs/notes/discovered/2026-10-02-vscode-secondary-sidebar-floor.md`). `@types/vscode` is pinned to that version, so the typecheck rejects a newer API.

The extension runs in Restricted Mode windows too; there its `escurel.*` settings are read from your user settings only (a folder cannot redirect you to another gateway).

Press F5 in VS Code (`editors/vscode` as the workspace) for the Extension
Development Host; point `escurel.gatewayUrl` at a running `escurel-server`.
