# Electron (VS Code) e2e under Xvfb hangs before it paints when the GPU driver is wedged

**Symptom.** The live e2e suite (`editors/vscode/test/e2e`, Playwright over CDP against a real VS Code
window under Xvfb) reported "could not attach to the VS Code window" or hung until the fixture's
timeout, on a workstation whose GPU driver had wedged after days of uptime. Nothing in the extension
had changed; the same run passed in CI and on a rebooted machine.

**Fix.** The e2e fixture (`editors/vscode/test/e2e/fixtures.ts`) appends
`ESCUREL_E2E_EXTRA_CODE_ARGS` to VS Code's launch arguments: `--disable-gpu` makes Electron paint in
software. The integration harness has the same hook as `ESCUREL_TEST_EXTRA_LAUNCH_ARGS`
(`editors/vscode/test/integration/runTests.ts`), and the demo launcher passes
`ESCUREL_DEMO_CODE_ARGS="--ozone-platform=x11 --disable-gpu"` (`editors/vscode/demo/run.sh`). Xvfb
itself is started with `-displayfd 3`, so it picks a free display and the fixture waits for it instead
of sleeping on a guessed `:99`.

**How to recognise it.** The window never appears over CDP, `Xvfb` is alive, and
`google-chrome --headless` / Playwright's Chromium also fail to deliver frames (see the component-test
note of the same day). A reboot clears the driver; the flag works around it until then.
