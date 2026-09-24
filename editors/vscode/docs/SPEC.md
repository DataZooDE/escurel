# escurel VS Code extension — implementation spec

**Date:** 2026-09-23 · **Status:** Ready for implementation · **Target:** `DataZooDE/escurel`, `editors/vscode/`
**Inputs:** `../2026-09-19-escurel-vscode-workbench/` (concept), `../2026-09-22-escurel-workbench-backend/` (BRD/HLD, implemented on `main`), `mock/Escurel Workbench Hi-fi.dc.html` (interactive hi-fi reference; open in a browser).
**Depth:** Lean. Boundaries, contracts and acceptance criteria are fixed here. Internal structure is up to the implementer.

---

## 1. Decisions (fixed)

- **Native first.** TreeViews, FileSystemProvider, `vscode.diff`, the Comments API, QuickPick and editor title actions. Webviews **only** for the thread graph, run detail and page-as-UI.
- **Webviews:** Lit web components, one bundle, esbuild. Theming uses **only** `--vscode-*` tokens, with no brand fonts or colours. Must work in light, dark and high-contrast.
- **Client:** the official MCP TypeScript SDK (streamable HTTP to `/mcp`) behind a thin typed wrapper (`src/client/`). Write the types by hand from `crates/escurel-server/src/mcp/schema.rs`. The wrapper is the only place that knows tool names.
- **Auth:** a VS Code `AuthenticationProvider` (`escurel`) doing OIDC with the gateway's issuer: PKCE via `vscode.env.asExternalUri` loopback, with device code as fallback. Tokens live in `SecretStorage`, and one refresher is shared by every HTTP and WS client.
- **Connection:** one gateway, one tenant (`escurel.gatewayUrl` setting). Online only: no cache, no offline queue. When disconnected, views show an empty state with Reconnect.
- **Live updates:** each view owns its own WS (`/ws`) with its own `event_subscribe` filter and resumes with `since_event_id`. Surface `session_cap_reached` as a non-blocking warning and fall back to refresh-on-focus for that view.
- **Instances:** edited only in the page-as-UI webview. Fields and body are editable. Raw markdown is shown read-only (`escurel:` FileSystemProvider, `isReadonly`).
- **Editing = live draft.** There is no Save. Every edit is a CRDT op on a _personal_ live draft (see §7, a backend prerequisite). Promote from review.
- **Skills:** raw markdown in a normal text editor via the `escurel:` FileSystemProvider (read-write). Save = `update_page` with `base_sha256`. `validate` runs on change (debounced) and maps issues to Diagnostics.
- **Review:** one surface for agent changesets **and** human live drafts. Opening an item shows `vscode.diff` (base ↔ proposed) with Promote / Discard as editor title actions and Comments API threads. Comments are events (`label_skill: review-comment`, assigned to the draft's target page).
- **Distribution:** a VSIX built in CI and attached to GitHub releases.

## 2. Vocabulary and colour roles

Four first-class nouns, each with **one** semantic colour, used on the chip, the node, the tree icon and the split button everywhere:

- **Skill:** `charts.purple`
- **Instance:** `charts.blue`
- **Event:** `charts.orange`
- **Run:** `charts.green` while running, `errorForeground` when failed or dead-lettered.

Buttons that start work are **Skill buttons**: a split button in the Skill colour. The primary label speaks from context, e.g. "Reassess risk for GH‑4711 with an agent". The dropdown holds **Start in background · First make a plan · Start in terminal · View skill**.
**Instance links** use the same split-button shape in the Instance colour: primary is **Open instance**, secondary is **View skill**. Keep the wording identical across all surfaces.

## 3. Surfaces

The activity-bar container `escurel` holds the views below. Tab and label wording follows the mock.

1. **Knowledge** (TreeView): Skills → Instances. Instances load through the `list_instances` cursor. Skill nodes show autonomy (`auto | review | confirm`) and layer, and are read-only when `layer_read_only`. Context menu: Open instance, View skill, Start skill…
2. **Inbox** (TreeView): `list_inbox`, newest first. **Awaiting you** sits on top and merges `list_changesets` (open), `list_drafts` without a changeset, `confirm` gates and your own live drafts. Selecting an event opens its thread.
3. **Runner** (TreeView, secondary sidebar): the latest `escurel:runner-status` (`list_events(label_skill, newest_first, limit 1)`), showing health, harness default, quotas used/limit, live runs, dead letters with reason, and paused tenants. Actions: Cancel, Retry, Requeue, Pause, Resume.
4. **Page as UI** (webview CustomEditor on `escurel:` instance URIs): header with the Skill link button, a typed field form (`fields[]`), `summary`, and the body (live draft). Also a thread strip (the root event → run that produced this version), the skill's `actions` as Skill buttons, the gate state and a Raw toggle (opens read-only markdown).
5. **Thread** (webview): `list_lineage(root_event_id)`, folded into a tree (event → run → changeset → draft / cascade event). Per the mock, the tree layout is _not_ swimlanes. Nodes are clickable: an event opens the thread, a run opens run detail, a draft/changeset opens review, and an instance opens page-as-UI. Inline gate buttons: Promote / Discard. Live via `event_subscribe{root_event_id}`.
6. **Run detail** (webview panel): attempts timeline, harness/model, status, the latest plan (`report_progress` snapshots) with step states, `get_run_tool_calls` (tool, ok/error_code, duration, bytes; paged by `after`), summary, trace id (copyable). Cancel / Retry. Live via `event_subscribe{run_id}`.
7. **Review:** see §1. Changeset = QuickPick of its drafts → a diff per draft. Promote all / Discard all on the changeset. `base_moved` shows a warning and a Re-draft hint; `already_decided` refreshes to the final state.
8. **Search / resolve:** `escurel.search` QuickPick (`search`, `granularity: page`), plus `escurel.resolve` on `[[skill::id]]` under the cursor, and a DocumentLinkProvider in skill markdown.
9. **Start a skill:** capture a user event with `label_skill`, `instance_page_id`, `source: workbench` and `provenance.manual {harness?, mode: run|plan, requested_by}`.
   - **Plan** opens run detail and waits for `status: planned`, then offers **Approve plan** (a new capture with `approved_plan_run_id`).
   - **Terminal:** `mint_agent_token(skill, target_page_id, root_event_id?)` → open an integrated terminal with `ESCUREL_URL`, `ESCUREL_TOKEN` and `TRACEPARENT` set and the configured harness command (`escurel.shellHarness`, default `claude`).

Controls (§3.3, §3.6) are `capture_event(label_skill: escurel:run-control, body {action, run_id|event_id|tenant, reason})`. A `permission_denied` result shows the reason and does not reveal anything.

## 4. Commands (minimum)

`escurel.signIn`, `escurel.signOut`, `escurel.search`, `escurel.resolve`, `escurel.openThread`, `escurel.openRun`, `escurel.startSkill`, `escurel.promote`, `escurel.discard`, `escurel.cancelRun`, `escurel.retryRun`, `escurel.requeue`, `escurel.pauseDispatch`, `escurel.resumeDispatch`, `escurel.showRaw`, `escurel.refresh`.

## 5. Layout in `editors/vscode/`

```
package.json            contributes: views, viewsContainers, commands, menus, customEditors, authentication, configuration
src/extension.ts        activation, wiring
src/auth/               AuthenticationProvider (PKCE + device code), token refresher
src/client/             MCP SDK transport, typed wrapper (one fn per tool), WS client (event_subscribe, resume)
src/fs/                 escurel: FileSystemProvider (skills rw, instances ro)
src/views/              Knowledge, Inbox/Awaiting, Runner tree providers
src/review/             diff content provider, Comments controller, promote/discard
src/webviews/           host side: panel/custom-editor controllers, message protocol
webview/                Lit components (page-as-ui, thread, run-detail), shared theme via --vscode-*
test/                   vitest (client), web-test-runner (components), playwright (visual)
```

The host ↔ webview protocol is typed postMessage. Webviews never hold tokens; every call goes through the host.

## 6. Tests (required)

- **vitest:** the typed wrapper against recorded JSON-RPC fixtures, covering cursor paging, `already_decided`, `base_moved`, `permission_denied`, `session_cap_reached` and WS resume.
- **web-test-runner:** page-as-UI (field types, summary, actions), thread folding from a `list_lineage` fixture (including pruned subtrees) and run detail (plan states, tool-call paging).
- **Playwright visual checks** of the three webviews in light, dark and high-contrast, with no hard-coded colours (lint: no hex in `webview/`).

## 7. Backend prerequisites (gaps found against `main` @ be469ed)

Most of the BRD is already on `main`: `list_lineage`, `report_progress`, `get_run_tool_calls`, `mint_agent_token`, `kind: system`, `event_subscribe` filters, `autonomy: confirm`, `summary_missing`, `harness:`, `cascade:`, run-control and runner-status. The extension additionally needs:

- **PR‑1 Live personal drafts (blocks M1 editing).** A human needs a mutable, personal draft of an instance that is edited live over CRDT ops and landed by the existing promote path, with all its guards (write ACL, validate, `base_sha256` CAS, `already_decided`). Only the author can read or write it before promotion. It shows up in `list_drafts` / review like any draft, and `diff_draft` works on its current state. Shape is up to the backend implementer (e.g. a session on a draft vs a draft-targeting `open_session`). Until PR‑1 lands, the extension ships with editing disabled in page-as-UI (read-only form + "Start skill" only).
- **PR‑2 Action labels.** `actions:` is currently a string list of cascade-target skills. For contextual Skill-button labels the skill needs an optional label (and default mode) per action, e.g. `actions: [{skill, label?, mode?}]`, keeping the string form valid. Until then the extension derives labels: `"<Skill title> for <instance title> with an agent"`.
- **PR‑3 Review-comment events.** Confirm that `capture_event(label_skill: review-comment, instance_page_id: <draft target>, provenance.review {draft_id, line?})` is accepted for non-admin users (kind `user`) and filtered by page ACL. If it isn't, add it.
- **PR‑4 Field render hints (optional, M3+).** `fields[].render` pass-through. The extension falls back to `fields[].type`.

## 8. Milestones

Each milestone ends with a VSIX release and passing tests.

- **M1 — Connect, tree, page, search.** Auth provider, typed client, FileSystemProvider, Knowledge tree, page-as-UI (read-only until PR‑1, then live-draft editing), skill editor with validate diagnostics, search/resolve, theme in all three modes.
  _Done when:_ you can sign in, browse skills → instances, open an instance as UI and raw, edit and save a skill with diagnostics, find a page by search, and it looks correct in light, dark and HC.
- **M2 — Inbox, Awaiting you, review.** Inbox and Awaiting views, the diff + Comments review, promote/discard for drafts and changesets, and human live drafts in the same queue (needs PR‑1, PR‑3).
  _Done when:_ an agent changeset of 3 drafts can be reviewed, commented, promoted in one action, and the Awaiting count updates live.
- **M3 — Thread and runs, live.** Thread webview (`list_lineage` + WS), run detail (plan, attempts, tool calls), node navigation across all surfaces.
  _Done when:_ a cascade E1 → run → changeset → promote → E2a‑c is visible as it happens, without reload, and each run opens with its plan and tool calls.
- **M4 — Runner and starting skills.** Runner sidebar, controls, Skill split buttons on page-as-UI and thread, start in background/plan/terminal, and approve plan.
  _Done when:_ you can start a skill from an instance, see the run in the thread, cancel it, retry a failed one, requeue a dead letter (admin), and a terminal-started harness appears as a governed run.

## 9. Non-goals

Multiple gateways or tenants, offline mode, Marketplace publishing, brand theming, swimlane thread views, rendering peacock blocks (render plain markdown tables; defer rich blocks).
