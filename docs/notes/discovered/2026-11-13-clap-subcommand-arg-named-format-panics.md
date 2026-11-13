# A subcommand argument named `format` panics the escurel CLI at runtime

**Symptom.** A new `escurel skill export --format skill-md` compiled, passed `cargo check`, and then panicked
the moment the command ran: `Mismatch between definition and access of 'format'`.

**Cause.** The CLI already defines a global `--format` (output switch: text/json). clap derives both into the
same argument id; the two definitions disagree on type, and clap only notices when the value is accessed.

**Fix.** Name the subcommand's flag something else — the SKILL.md export uses `--as skill-md`. When adding a
flag to any `escurel` subcommand, check the global args in `crates/escurel-cli/src/main.rs` first.

**Recognise it.** A clap "Mismatch between definition and access" panic on a command that compiles cleanly; run
every new subcommand once in a test, not just its parser.
