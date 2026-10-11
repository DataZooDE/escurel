# Action-validation dedupe review (2026-10-11)

Conclusion: **not a clean candidate — no refactor.** The five host-side
webview-message validators share a *shape* ("check what the webview sent
against what the host holds") but differ in semantics, trust level, and
return type. Forcing them into one helper would change behaviour or add
adapters as long as the code removed. Evidence below; all line numbers
are under `editors/vscode/src/`.

## The five validators

1. `runs/runActions.ts:24-45` (`resolveRunAction`) — the run panel.
   The **only** site that rejects unknown keys (`:33` for `view-skill`,
   `:40` for `run-control`, with the comment "Extra fields are never
   accepted, even if they happen to match host data").
2. `thread/detailsRouting.ts:28-70` (`acceptDetailsAction`) — the
   Details-panel envelope gate. Per its own docstring (`:23-27`) it is
   deliberately coarse: hold the message to the shown node, then
   "validated again, against that thread's loaded state, by
   `resolveThreadAction`". It does **not** reject unknown keys: e.g. a
   `run-control` for the shown run with an extra `foo` key passes `:42-44`
   (only `eventId` is refused). The same extra key is rejected by (1).
   That difference is layered defence, not duplication.
3. `thread/inspectorActions.ts:138-274` (`resolveThreadAction`) — takes an
   already-typed `ThreadWebviewToHost`, so key-allowlisting is meaningless
   here; it checks lineage membership (`view.nodes.find`, `:167-173`,
   `:217-221`) and recomputes offered controls from host lineage.
4. `shared/hostMessages.ts:7-31` (`skillPageMessageAllowed`) — returns
   `boolean` (the others return commands or `undefined`), against
   `SkillPageModel`; its `start-skill` modes are `run | plan` (`:18`),
   while the page validator's are `background | plan | terminal`
   (`editors/pageMessages.ts:3`).
5. `editors/pageMessages.ts:20-69` (`resolvePageMessage`) — the `open-*`
   arms (`:57-65`) deliberately accept **any** non-empty string
   (documented as reads, `:17-18`). Imposing host-held-id checks there,
   or key-allowlisting on (2)/(3), would be a behaviour change, and several
   existing tests pin the current leniency (e.g.
   `test/unit/pageMessages.test.ts:81-90`).

## Closest near-miss, and why it stays

The "find enabled offered control + resolve it" tail appears in both
`runActions.ts:41-44` and `inspectorActions.ts:230-243`. But the inputs
differ (`factsOf(view)` from an `ActionRunView` vs `threadRunFacts`
from lineage + `rawById` + admin), the outputs differ
(`{ command, args: object }` vs `{ command, args: [arg] }`), and the
thread arm threads `warn` callbacks through each refusal. A shared helper
would need `warn` + args-shape parameters — the adapter is the code.
Sharing already exists at the right layer: both call `offeredControls` /
`resolveControl` / `visibleRunControls` from `runs/runFacts.ts`.

## Genuinely duplicated, out of scope

The 2-line `isRecord`/`isObject`/`asRecord` record-guard is copy-pasted in
six files (`thread/detailsRouting.ts:17-18`, `commands/nodeRefs.ts:13-14`,
`views/planRows.ts:18-21`, `shared/rowSource.ts:24-25`,
`shared/report.ts:45-46`, `shared/preview.ts:47-48`). Real textual
duplication, but it is not action validation and unifying it means
touching six unrelated modules for a one-liner — left alone.

## Verification

No source touched. Ran the five suites that pin the behaviour above —
50/50 green (`npx vitest run` on `runActions`, `detailsRouting`,
`hostMessages`, `inspectorActions`, `pageMessages` tests), confirming the
divergent semantics (forged id / stale id / extra key / disabled control
refusals) are characterised and must stay as-is.
