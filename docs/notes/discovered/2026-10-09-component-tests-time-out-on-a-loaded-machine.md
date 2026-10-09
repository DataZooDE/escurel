# web-test-runner component tests time out on a loaded machine (and a wedged GPU)

**Symptom.** The VS Code extension's component suite (`npm run test:component`, web-test-runner +
Playwright Chromium) failed 17–27 tests with timeouts on the owner's workstation while the load average
was 60–70 (several Rust builds and e2e runs in parallel), and the same tests passed on an idle machine
and in CI. A second flavour, seen with a wedged GPU driver: Playwright's bundled Chromium never delivers
an animation frame, so every test that awaits `requestAnimationFrame` times out regardless of load.
The pre-push hook runs this suite, so a push kept failing although the branch was fine.

**Fix.** Three opt-in knobs in `editors/vscode/web-test-runner.config.mjs`; unset, nothing changes and
CI is unaffected:
- `ESCUREL_WTR_TIMEOUT_MS` raises mocha's per-test timeout (default 2 s).
- `ESCUREL_WTR_FINISH_MS` raises the runner's whole-run limit (default 120 s).
- `ESCUREL_WTR_CHROME=/usr/bin/google-chrome-stable` points the runner at an installed Chrome when the
  bundled one cannot paint.

**How to recognise it.** `uptime` shows a load average far above the core count; the failures are
pure timeouts with no assertion message; they vanish when the box is idle. Do not reach for
`--no-verify`: set the knobs and retry the push.
