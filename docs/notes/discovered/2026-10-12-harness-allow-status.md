# Runner publishes its harness allow-list (BACKEND_GAPS PR-10)

Date: 2026-10-12.

## Symptom

`escurel:runner-status` carried only the DEFAULT harness (`harness`), so the
extension could not offer a picker of valid harnesses: a start either sent
nothing or sent `escurel.harness` blind, and a name outside
`ESCUREL_RUNNER_HARNESS_ALLOW` failed the run (`harness not allowed`).

## Fix

- Runner (`crates/escurel-runner/src/main.rs`, `status_snapshot`): the status
  body now carries `harness_allow` next to `harness` — the configured
  allow-list, sorted, names only (never paths, commands, env or secrets).
  Unset `ESCUREL_RUNNER_HARNESS_ALLOW` still publishes the configured harness
  alone. Covered by
  `crates/escurel-runner/tests/suite/harness_allow_status.rs` (real gateway +
  real runner binary): sorted equality, names-only shape, exact body key set
  (no extra detail leaks), and the default-alone case.
- Extension: `RunnerStatusBody.harness_allow` + `allowedHarnesses()` sanitiser
  (`src/views/runnerModel.ts`: `cleanText`, dropped when over 40 chars, capped
  at 20 names, `undefined` on an old runner); pure
  `chooseHarness(settings, status) -> {ask|send|refused|none}`
  (`src/start/chooseHarness.ts`); the start flow (`pickHarness` in
  `src/start/startSkill.ts`) shows the QuickPick only when the setting is
  blank and the list holds more than one name (`Use default` first, nothing
  remembered) and never sends a name outside the published list (a configured name that is not on it is a visible warning with a choice, not a silent fallback); the Runs
  dispatch tooltip lists the allowed harnesses.
- Docs: `editors/vscode/docs/BACKEND_GAPS.md` PR-10 closed,
  `editors/vscode/docs/SPEC.md` §9, skill reference
  `references/11-event-driven-agents.md` (runner-status body + picker).

## How to recognise it next time

A consumer needs a choice among server-side names: publish the list in the
status the consumer already reads, sorted and names-only, and gate the sender
on it — never let the sender guess names the runner will refuse.

## Changelog lines for the release notes

(Shared changelog files untouched — three parallel tasks would conflict.
Fold these into the release notes on merge.)

- Skill (`escurel-platform` CHANGELOG): "Runner status publishes
  `harness_allow`: the workbench offers the allowed harnesses as a picker on
  start when `escurel.harness` is unset, and never sends a harness outside
  the published list (a configured name not on the list gets a warning and a
  choice instead of a silent fallback)."
- Root CHANGELOG: "BACKEND_GAPS PR-10 closed: `escurel:runner-status` body
  carries sorted `harness_allow` (names only); extension start flow picks
  from it."
- `docs/notes/breaking-wire-changes.md`: no entry — additive field; old
  runners (field absent) keep today's send-none behaviour.
