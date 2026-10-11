# Calm audit nits and first-connect focus suggestion

The quiet Overview fixture overwrote every headline with “All clear”, masking the model's specific
empty states. Build the quiet fixture through `buildOverview` and assert the finished/records wording
in both model and component tests so the screenshot data cannot silently drift again.

Details put a heading above only the right field group, offsetting the first field row. Share the
heading/field grid rows with subgrid, baseline-align each definition list, and filter whitespace-only
values before deciding whether to render any field group. Component tests cover both omissions and
matching first-row positions. An empty body also no longer leaves a Summary heading behind.

Focus mode had commands but no first-connect suggestion hook. Services changes are not evidence of a
connection; listen to LiveCoordinator's successful socket connection, verify a signed-in subject, and
ignore a socket replaced during that lookup. Keep the decision pure and the orchestration awaitable.
Claim the suggestion synchronously and persist before displaying so simultaneous connection callbacks
cannot duplicate it. Demo bootstrap can connect BEFORE its silent entry command: detect both bootstrap
extensions as well as silent entry, active focus, web UI and untrusted/virtual workspaces. Unit tests
exercise all decisions, dismissal/buttons, persistence, reconnects and a replaced connection without
sleeps.

Validation: unit tests, typecheck, lint and the component suite run green (the component tests were
executed by the reviewer, outside Codex's restricted sandbox). The low-zoom thread-overview change that Codex
also made (icon-only cards) was NOT taken: the owner's design keeps icon AND text at low zoom; the header
overlap at 40% is tracked separately.

Regenerate in light, dark, high-contrast and calm:

- `overview-quiet.png` (specific empty-state wording).
- `thread-inspector-run.png`; also review/regenerate `thread-inspector-draft.png` if its fixture has
  fields (shared grid rows and omission of empty values).

`overview-morning`, `thread-lanes` (100%), and `details-empty` should remain unchanged.

Changed files (relative to `editors/vscode`):

- Overview: `src/overview/model.ts`, `test/unit/overviewModel.test.ts`,
  `test/component/overview-fixtures.ts`, `test/component/overview.test.ts`.
- Details: `webview/thread/inspector.ts`, `test/component/thread-inspector.test.ts`.
- Focus: `src/focus/focusSuggestion.ts`, `src/focus/registerSuggestion.ts`,
  `src/focus/focusMode.ts`, `src/live.ts`, `src/extension.ts`,
  `test/unit/focusSuggestion.test.ts`, `test/unit/focusSuggestionHost.test.ts`.
- Documentation: `docs/SPEC.md`, `media/walkthrough/signin.md`, and this note.
