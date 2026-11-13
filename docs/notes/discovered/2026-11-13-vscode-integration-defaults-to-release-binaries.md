# VS Code integration tests run whatever is in `target/release`

**Symptom.** `npm run test:integration` in `editors/vscode` passes (or fails) for reasons that
have nothing to do with the code you just changed — e.g. a steer test kept passing after the
gateway changed the `run-finished.transcript` shape, then failed with `event_not_found` for a
gate change made hours earlier. Separately, the cascade suite's gateway-switch tests fail with
`the harness must provide ESCUREL_TEST_GATEWAY_BIN`.

**Cause.** `test/integration/runTests.ts` defaults `ESCUREL_SERVER_BIN`, `ESCUREL_RUNNER_BIN`
and `ESCUREL_TEST_GATEWAY_BIN` to `target/release/…` and silently uses whatever binaries are
there, however stale. `gateway-switch.test.ts` reads `ESCUREL_TEST_GATEWAY_BIN` from the
extension host's *process* env, which only CI (`vscode.yml`) sets; the runner's default path is
not forwarded.

**Fix / how to run it.** Rebuild first (`cargo build --release -p escurel-server -p
escurel-runner -p escurel-test-support`), then pass all three explicitly:

```sh
R=$PWD/../../target/release
ESCUREL_SERVER_BIN=$R/escurel-server ESCUREL_RUNNER_BIN=$R/escurel-runner \
ESCUREL_TEST_GATEWAY_BIN=$R/escurel-test-gateway ESCUREL_REQUIRE_CASCADE=1 \
  npm run test:integration
```

`ESCUREL_REQUIRE_CASCADE=1` turns a missing binary into an error instead of skipped suites.

**Recognise it.** A wire-shape change in the gateway that the extension's integration tests
"didn't notice", or green results that a teammate cannot reproduce: check the binaries' mtime
against your last commit.
