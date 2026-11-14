# A free port handed to a child process can be taken before it binds

**Symptom.** Under parallel load, a `teams_roundtrip` test failed and `escurel-chat-bridge` exited with
`AddrInUse`. Each test asked `escurel_test_support::free_port()` for a port and passed it in the
environment of a spawned bridge. The same tests passed when run alone.

**Cause.** `free_port()` binds port 0, reads the number the kernel picked, and closes the socket. The
port is free at that moment, but nothing reserves it. A spawned child binds it only when it starts,
which can be seconds later under load. In that window, another test's listener or child process can
take the same port. More tests per binary make the window more likely, and this flake was older than
the tests that exposed it.

**Fix.** In `crates/escurel-chat-bridge/tests/teams_roundtrip.rs`, a minted bridge that exits during
startup with `AddrInUse` is spawned again on a fresh port, a few times at most. In-process listeners
are not affected: they bind port 0 themselves and report the address they got.

**How to recognise it next time.** A child process (gateway, runner, bridge) in a multi-test binary
dies at boot with `Address already in use`, only in the full run. Where the child can, let it bind port 0 and
report the address it got. Otherwise, retry the spawn on `AddrInUse`. Don't add sleeps.
