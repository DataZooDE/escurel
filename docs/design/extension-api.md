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
2. Extending the generic page view additively (extra sections, actions, forms inside `page-as-ui.ts` +808): the renderer hook replaces the whole view, it does not extend it. Needs an "augment" hook if wanted.
3. Review controller (`src/review/controller.ts`: decision note, recorded decisions, impact preview, accept-with-edits). A review hook (before/after decision) would be needed; otherwise companion-only.
4. Inbox decoration (`src/views/inbox.ts`, `inboxModel.ts`: request statuses, badge, localized empty message). Needs a row-decorator hook, or stays companion-only.
5. Webview-side shared helpers used by companion webviews (`webview/shared/markdown-view.ts` options, `download.ts`, `filter-strip.ts`, `vscode-api.ts` message union). The companion bundles its own copies or Escurel exports a webview helper bundle.
6. Visual identity: brand tokens in `webview/shared/theme.css.ts` (fonts, radii, surfaces, muted colour), font loading and CSP `font-src` in `webviewHtml.ts`, `media/fonts`, `themes/*`, `focusSettings.ts` (theme and window title), `esbuild.mjs` font copy. Companion-only; `webviewHtml` would need a `fontSrc`/extra-head option for the fonts.
7. `src/views/scenarios.ts` / `scenariosVisibility.ts` (view visibility by setting) and `src/shared/untrustedText.ts` (U+2028/2029 as unsafe): the second is a generic fix and can go upstream on its own.
8. `package.json` (+741 lines: views, commands, menus, configuration), `demo/`, `test/e2e/fixtures.ts`, `docs/SPEC.md`: the companion's manifest and harness.

## Tests

`test/unit/extensionApi.test.ts`, `contributedTiles.test.ts`, `pageRendererDelegation.test.ts`, `themeTokens.test.ts`,
`apiExposure.test.ts`. Not covered by unit tests: the Overview panel wiring in a real window (integration suite).
