# Escurel VS Code extension: API for companion extensions

Status: implemented in `editors/vscode/src/api` (API version 1). Types: `editors/vscode/src/api/types.ts`.

## Problem

A product that sits on top of Escurel (a "companion extension", for example `acme.example` with
`extensionDependencies: ["escurel.escurel"]`) needs its own views and webviews inside the Escurel window,
calling the gateway with the user's session. Without a public API it can only patch the Escurel extension
(about 40 modified files in one known fork), which makes every Escurel release a merge.

## Decision

`activate()` returns a small, versioned object in **every** mode. The service graph (token store, controllers)
that `activate()` used to return stays test-only: it is merged in only under Development and Test mode, as before
(`src/shared/apiExposure.ts`).

```ts
const escurel = vscode.extensions.getExtension<EscurelExtensionApi>('escurel.escurel')!;
const api = await escurel.activate();
if (api.apiVersion !== 1) return; // refuse a major it does not know
```

| Member | Purpose | Rules |
| --- | --- | --- |
| `apiVersion: 1` | Compatibility check. | Breaking change bumps it. Additions do not. |
| `getClient(tenant?)` | `GatewayClient.callTool(name, args)`: one MCP tool call with the user's session. | Host side only. No token, no URL, no typed methods. `tenant` is a gateway URL; one window talks to one gateway, so any other value throws. The caller's input is validated again (tool name pattern, plain-object args); the gateway still decides what the user may do. The client is resolved per call, so a rebuilt client is used at once. |
| `webviewHtml(opts)` | Shell HTML for a companion webview: nonce-gated module script from `<companion>/dist/webview/<bundle>.js`, CSP `default-src 'none'`, no network, theme tokens on `:root`. | `bundle` and `tag` are pattern-checked, since they land in markup. Same helper the core views use. |
| `themeTokens`, `themeCss` | The `--escurel-*` tokens (noun colours, border, muted) as data and as a `:root` rule. | Theme variables only, no literal colours. A test keeps them equal to `webview/shared/theme.css.ts`. |
| types-only entry | `src/api/types.ts` re-exports the protocol types a companion may share (`PageModel`, `FieldView`, `Overview*`, host/webview page messages) and defines the API types. | `import type` only; emits nothing but the version constant. A companion pins an Escurel commit or release and reads this file from it. |
| `registerOverviewTile(tile)` | Adds a tile to the Overview board: `{ id, title, openCommand?, load(ctx) }`; `load` returns data (`headline`, `tone`, up to 5 `items`, `more`, `empty`). | The core board renders the tile; no companion script runs in the Overview webview. Lines carry only a key; the host keeps `command`/`args` in its action table. Text is cleaned and capped like gateway text. A provider that throws or exceeds 5 s shows an "unavailable" tile. Tile id on the wire is `ext:<id>`. Duplicate id throws; `dispose()` removes it and the open board reloads. |
| `registerPageRenderer(skill, renderer)` | `renderer.render({ pageId, skill, frontmatter, panel, client, onDidChangeTenant })` takes over the page-as-UI editor panel for instances of `skill`. | The renderer sets `panel.webview.options` and `html` (use `webviewHtml`) and talks to its own webview with its own typed messages, re-validating them on the host. A throwing renderer falls back to the generic page view. With no renderer registered the skill is not read. One renderer per skill; duplicate throws. |
| `onDidChangeTenant` | `Event<{ gatewayUrl }>`: the gateway changed or the signed-in identity changed. | Drop everything cached from the old connection. |

Deliberately left out: sessions or tokens, a way to run tools the user could not run, access to core
controllers, a webview-side library. Custom drawing inside the Overview board is out of scope: a companion opens its own
panel (`webviewHtml`) from a tile's `openCommand`.

## Additive hooks

These extend a core view instead of replacing it, so a companion keeps no copy of the view. All are additive to
API version 1 (a companion that does not use them is unaffected) and share one rule set: providers and hooks run
on the host, what they return is data that the host cleans and bounds, the core webview draws it with text nodes
only, commands and tool names never reach a webview, and the host re-validates every message from a webview.

### Page view augment: `registerPageAugment(skill, augment)`

```ts
api.registerPageAugment('acme-thing', {
  sections: async (ctx) => [{ id: 'impact', title: 'Impact', blocks: [{ type: 'table', columns: [...], rows: [[...]] }] }],
  actions: (ctx) => [{ id: 'open', label: 'Open report', command: 'acme.example.openReport', args: [ctx.pageId] }],
  forms: [{ id: 'note', title: 'Add a note', fields: [{ name: 'text', label: 'Note', kind: 'textarea', required: true }],
            toDraft: (values, ctx) => ({ target_page_id: ctx.pageId, content: ... }) }],
});
```

| Part | Rules |
| --- | --- |
| `sections(ctx)` | Returns `{ id, title, blocks, empty? }`. Block types: `text`, `facts` (label, value, tone), `table` (columns, rows of strings), `list` (lines that may name a host `command` and `args`). Anything else is dropped. At most 12 sections, 8 blocks per section, 50 rows, 8 columns, 20 list lines; text is cleaned like gateway text. A tone is shown in words as well as colour. |
| `actions(ctx)` | Returns `{ id, label, hint?, command, args? }`. The command id is pattern-checked; the webview gets only a handle (`a:<n>`, `l:<n>` for list lines) and posts it back, and the host runs the command it holds for that handle. A handle it did not hand out is refused. |
| `forms` | Static list of `{ id, title, submitLabel?, fields, toDraft }`. Field kinds: `text`, `textarea`, `enum`, `bool`, with `required`, `values`, `maxLength` (default 2000, at most 20000). The host re-validates the submitted values against its own copy of the form (required, enum membership, length, unknown keys dropped), calls `toDraft` on the host, and keeps only `target_page_id`, `content` and a 64-hex `base_sha256`; the call is `create_draft` and nothing else, so a reviewer still decides. `target_page_id` must be a relative page id without `..`. |
| `ctx` | `{ pageId, skill, frontmatter, client, signal }`. `client` is the same `GatewayClient`. `signal` aborts after about 5 s. |
| Failure | A provider that throws or times out costs only its own contribution; the view says "Some additional information could not be read just now". A throwing `toDraft` answers the form with a refusal. |
| Scope | Several companions may augment one skill (unlike `registerPageRenderer`); `dispose()` removes one. A skill with a registered renderer is drawn by the renderer, not augmented. With no augment registered for a skill, nothing extra is read. |

Not offered: scripts in the webview, a form that writes anything other than a draft, a button that names a tool.

### Review decision hooks: `registerReviewHook({ before?, after?, preview? })`

```ts
api.registerReviewHook({
  before: (ctx) => ctx.drafts.some(isMine) ? { note: 'required', notePrompt: 'Why accept this change?' } : undefined,
  after: async (ctx) => { await ctx.client.callTool('capture_event', { ... ctx.draftId, ctx.decision, ctx.note ... }); },
  preview: async (ctx) => isMine(ctx.targetPageId) ? [{ id: 'impact', title: 'Impact', blocks: [...] }] : [],
});
```

| Member | Rules |
| --- | --- |
| `before(ctx)` | Runs before accept (`escurel.promote`), reject (`escurel.discard`) and accept with edits. `ctx = { decision, drafts: [{ draftId, targetPageId }], client, signal }`; a changeset decision lists its open drafts. May return `{ note?: 'required' \| 'optional', notePrompt?, veto? }`. The strictest note over all hooks wins; the reviewer gets one input box (a reject reuses its own reason box, so it is never asked twice) and Escape cancels the decision. A non-empty `veto` string stops the decision and is shown as a warning. |
| `after(ctx)` | Runs once per decided draft after the gateway call succeeded (not for a failed or already decided one). `ctx = { draftId, targetPageId, decision, note, provenance, client }` with `decision` in `accepted`, `rejected`, `accepted_with_edits`, `note` the reviewer's text (empty when none was asked) and `provenance` the draft's own author, run, thread, event, changeset, creation time and hashes as strings. A changeset accept reports only the drafts applied now. |
| `preview(ctx)` | Returns sections in the schema of the page augment (read-only; list lines may open a host command; no forms). When any hook returns a section, a panel "Review context" opens beside the diff of that draft (core webview, same rules as the page augment); with none, no panel is opened. `ctx = { draftId, targetPageId, client, signal }`. |
| Failure | A hook that throws, answers nonsense or exceeds 5 s counts as no answer (logged). Only an explicit `veto` blocks; an `after` error never undoes or hides a decision. |
| Accept with edits | The core has no editing UI. A companion that offers one saves its edit and runs `escurel.promote` with `{ draftId, withEdits: true }`; the hooks then hear `accepted_with_edits`. |
| Scope | Hooks see every decision; a hook that cares about one skill or page pattern checks `targetPageId` itself. Several hooks run in registration order. |

### Inbox row decoration: `registerInboxDecorator(skill, decorator)`

```ts
api.registerInboxDecorator('acme-request', {
  decorate: (row, rows) => ({ badge: 'Answered', description: 'by the agent', status: 'answered' }),
  emptyMessage: { en: 'No requests yet.', de: 'Noch keine Anfragen.' },
});
```

| Part | Rules |
| --- | --- |
| `decorate(row, rows)` | Called for each Inbox row whose event is filed under `skill`. `row` and `rows` (every visible row, for correlation: a reply row can mark its request answered) are plain data with string values only (`eventId, labelSkill, title, body, pageId, at, rootEventId, runId, eventStatus, provenance`). Returns `{ badge?, description?, status?, hidden? }` (sync or a Promise). `badge` is a short word (24 characters) shown before the secondary text; `description` replaces the secondary text (skill and age); `status` is `open`, `answered`, `failed` or `done`: it picks the icon, and a row that is not `open` sorts after the others; `hidden` leaves a technical receipt out. |
| View badge | When a decorator is registered, the Inbox view badge shows the number of rows whose status is `open`. |
| `emptyMessage` | A string, or one text per language (`en`, `de`, ...); chosen by the window language (exact, language family, `en`). Shown as the view message when the Inbox has no row, so a requester sees where a request goes. The first decorator that contributes one wins. |
| Bounds and failure | Data only: no command, no script, no webview. Returned text is cleaned and capped; anything outside the schema is dropped. The first decorator that sets a field wins. A decorator that throws or exceeds 2 s leaves its rows as they were. Registering or disposing a decorator refreshes the open view. |

## Classification of a downstream fork's changes

Measured against the `editors/vscode` diff of one downstream fork: 298 files, 257 added, 41 modified.

Covered by a hook (the companion keeps its own code, Escurel carries nothing of it):

- `src/client/index.ts`, `src/client/types.ts` (append_message, list_messages, ingest_upload, paged changesets/drafts, get_draft, promote_draft with `content`): `getClient().callTool`, with the companion's own types. The typed `promoteDraft({content})` and paged-list methods are generic client features and may be upstreamed separately.
- `src/services.ts` (`connectionGeneration`): `onDidChangeTenant` (the companion counts events itself).
- `src/shared/webviewHtml.ts` (shell for companion webviews): `webviewHtml(opts)`. The custom brand fonts, `font-src`, `<html lang>`, `<title>` additions are not covered (below).
- `src/shared/protocol.ts` (+471 lines): the shared part via the types-only entry; the domain-specific message types move into the companion.
- `src/overview/*` simple "count and list" panels: `registerOverviewTile`.
- `src/editors/pageAsUi.ts`, `webview/page-as-ui/*` where a skill gets its own page UI: `registerPageRenderer`.
- `src/extension.ts` (registering the companion's controllers): the companion's own `activate`.
- Added domain-specific feature folders, their webviews, tests and media: companion code, no hook needed beyond the above.

Not covered (companion-only, or a follow-up if the owner wants it generic):

1. Rich Overview board (`src/overview/controller.ts` +1239, `webview/overview/overview.ts` +1658: import, scenarios, reference seeding, exports in one panel). Companion-only: it becomes the companion's own panel.
2. Extending the generic page view additively (extra sections, actions, forms inside `page-as-ui.ts` +808): covered by `registerPageAugment` (see Additive hooks).
3. Review controller (`src/review/controller.ts`: decision note, recorded decisions, impact preview, accept-with-edits): covered by `registerReviewHook` (see Additive hooks).
4. Inbox decoration (`src/views/inbox.ts`, `inboxModel.ts`: request statuses, badge, localized empty message): covered by `registerInboxDecorator` (see Additive hooks).
5. Webview-side shared helpers used by companion webviews (`webview/shared/markdown-view.ts` options, `download.ts`, `filter-strip.ts`, `vscode-api.ts` message union). The companion bundles its own copies or Escurel exports a webview helper bundle.
6. Visual identity: brand tokens in `webview/shared/theme.css.ts` (fonts, radii, surfaces, muted colour), font loading and CSP `font-src` in `webviewHtml.ts`, `media/fonts`, `themes/*`, `focusSettings.ts` (theme and window title), `esbuild.mjs` font copy. Companion-only; `webviewHtml` would need a `fontSrc`/extra-head option for the fonts.
7. `src/views/scenarios.ts` / `scenariosVisibility.ts` (view visibility by setting) and `src/shared/untrustedText.ts` (U+2028/2029 as unsafe): the second is a generic fix and can go upstream on its own.
8. `package.json` (+741 lines: views, commands, menus, configuration), `demo/`, `test/e2e/fixtures.ts`, `docs/SPEC.md`: the companion's manifest and harness.

## Tests

`test/unit/extensionApi.test.ts`, `contributedTiles.test.ts`, `pageRendererDelegation.test.ts`, `pageAugment.test.ts`, `pageAugmentWiring.test.ts`, `reviewHooks.test.ts`, `reviewControllerHooks.test.ts`, `reviewPreviewPanel.test.ts`, `inboxDecorator.test.ts`, `test/component/review-preview.test.ts`, `test/component/augment-view.test.ts`, `themeTokens.test.ts`,
`apiExposure.test.ts`. Not covered by unit tests: the Overview panel wiring in a real window (integration suite).
