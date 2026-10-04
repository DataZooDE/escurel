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
3. **Runs** (TreeView `escurel.runner`, secondary sidebar; the view, the commands and the docs all say "Runs", not "Runner"): the **runs control center** (owner decision of 2026-10-04: "control running agents, and get insight into historic runs and their traces"). It is built from the run lifecycle events (`list_events{label_skill: escurel:run}`, newest first, paged by cursor) folded per run, plus the latest `escurel:runner-status`, and it is live over `/ws` (`label_skill: escurel:run`; the runner heartbeat is polled every 15 s). The model is pure (`src/views/runsModel.ts`, unit-tested); the provider only fetches and maps rows.
   - **The view's message** says in words whether the runner is there (`Agents are running · last seen 3 s ago`, `Agents are not responding`, `No agents have reported yet.`; the engine, e.g. `Agent engine: echo (demo, no AI model)`, is only in the tooltip), and the active filter.
   - **Rows, in order:** an **agents** row (`Agents are running` / `Agents are paused`, with Pause/Resume as its inline button: admin-only, shown but deactivated for anyone else, who see "admins only" and the full sentence in the tooltip), an **insight** row of two short lines (`Last 24 h: 12 runs` / `11 ok · 1 failed · avg 6 s`, hidden when there is nothing to say), then the groups, each with its count: **Running now** (skill · target, the elapsed time ticking, inline Cancel), **Waiting for you** (a planned run: inline Approve plan), **Needs attention** (failed and dead-lettered runs; the reason is its own child row, `permanent — harness not allowed: x`; inline Retry, Requeue in the menu, deactivated with the reason for non-admins; the ten newest, then "Show N older failures", which filters History to failures) and **History** (everything that ended, newest first, 25 at a time with **Load more…**, which reads older events from the gateway when the loaded ones are shown). Waiting and Needs attention are hidden when empty. A run that was retried or re-planned disappears from Needs attention/Waiting once a newer run of the same trigger exists.
   - **A row** is `Outcome · skill · target` (`Failed · supplier-risk · order-4500152`: the OUTCOME comes first so a narrow panel cuts the page, never the word that matters; the skill is the label of the event that triggered the run, the target page's slug, never an id) with the duration and a short relative time as its description (`6 s · 3 m`, `1 h`, `12 s` while running). The icon AND the word say the state; the accessible name and tooltip carry the full wording and, only in the tooltip, the short run id. Click opens **Run detail**; the context menu offers Open thread, Open the page this run worked on and Copy run id.
   - **Filter:** the title action opens a QuickPick (Succeeded / Failed (incl. failed for good) / Cancelled / Today / Yesterday / Last 7 days (UTC days) / each skill seen / search by text); **Runs for this record** (a link on a record page and in the Knowledge menu) opens this view filtered to one page; a title action clears it. It narrows History only: what is running or needs you is never hidden by a filter.
   - **Bounds:** the model folds 5,000 events well inside a frame (unit-tested), only a page of History is turned into rows, and the first load reads at most 8 pages.
   - Actions: Cancel, Approve plan, Retry, Requeue, Pause, Resume, as in §3.9; Quotas (the admin-only `admin_quota`) are no longer shown here.
4. **Page as UI** (webview CustomEditor on `escurel:` instance URIs): header with the Skill link button, a typed field form (`fields[]`), `summary`, and the body (live draft). Also a thread strip (the root event → run that produced this version), the skill's `actions` as Skill buttons, the gate state and a Raw toggle (opens read-only markdown). The tab is named after the file (`skill__id.md`), because VS Code labels a custom editor by its URI; the record's title and skill are in the page itself (see `docs/notes/discovered/2026-10-04-tab-titles-follow-the-uri.md`).
   **Skill page** (webview CustomEditor on `escurel:/skills/<id>.md`, owner decision of 2026-10-04): `escurel.viewSkill` opens the skill as a readable page, not its Markdown: the title and what it is for, freshness in words, About (role, folder, tags, where the data comes from, what happens to an agent's changes, and where a shared skill comes from; each fact has a tooltip), its Fields (required or optional, what each holds; a section with nothing in it is left out and one line says so), what it can start (the `actions`, which ask for the record to work on), its first 10 records and its latest 8 events with a word for how each went. **Show Markdown** (a button in the page and the editor-title action) opens the source.
   **Review diff** (`escurel-review:`): the tab reads `<skill> · <page> — draft by <author>`; four title actions open the instance, thread, run and skill the change belongs to (a person's draft has no run, and says so). **Plan ready**: a plan run that ended `planned` and has no approval is a row in Awaiting you with an Approve plan action, so a dismissed toast loses nothing. **Notices**: every notification that names a page, run, thread or skill offers up to two Open buttons (one helper, `src/shared/notice.ts`). **Explain this view** (`escurel.explainView`, a `?` title action on Knowledge, Threads, Awaiting you, Inbox and Runs): one screen of plain words on how events, skills, runs, changesets and instances connect, plus a glossary (needs you, draft vs changeset, cascade, agent engine, autonomy, row/notes/source, dead letter, trace); it opens as a side preview titled for what it is. **Routine outcomes** (cancelled, retried, paused, applied) go to the status bar as one line; a notification is kept for what failed or needs the person. Cancel and Pause ask first ("Work already done is kept").
5. **Thread** (webview): `list_lineage(root_event_id)`, folded into a tree (event → run → changeset → draft / cascade event). Layout: columns are stages (root event, run · changeset, instances · drafts, cascade, outbound) and **rows are cascade branches** (lanes): the main chain is the first row, and each further follow-on event of a run starts a row of its own below it, so branches do not cross. This is not the mock's tree and not actor swimlanes (rows by who acts); it is an owner decision of 2026-10-03. A node that is finished with nothing left to do (a processed event or run, a decided changeset or page) is drawn as a small two-line card; live or waiting nodes keep the full card. Every card carries a type icon, a type word and a coloured accent bar (event, cascade, run, changeset, page), so type is never colour alone. **First view and zoom** (owner decision of 2026-10-03): a thread opens once; later live reloads never move the view. A graph that fits the canvas at 100% opens as it is. A bigger one opens at 100% with the node that matters centred: the first node that needs you (document order: main row, then lanes top to bottom, left to right), when nothing needs you, the thread opens at its ROOT (owner decision of 2026-10-04: a thread that opened on its newest node hid where the story starts); thin scrollbars on the axes that overflow make the cut-off edges reachable, and Fit still shows the whole graph. Below 70% zoom the cards switch to a low-zoom form (icon, accent bar, type word, title and state chip, plus the Needs-you icon; the body, meta lines and buttons are dropped) with an "overview" hint beside the zoom percentage. The words are counter-scaled so they render at about 10px at any zoom (owner decision of 2026-10-03: "icons and text, no body"); a title or chip that does not fit is cut with an ellipsis, never over a neighbour, the type word keeps its full width. Card boxes keep their size so wires do not move, and the accessible name and tooltip keep the full text. In that form the in-card buttons (Promote, Discard, Review changes, draft links, collapse) are not shown or focusable; Enter on a card opens it, and Promote/Discard stay available in Awaiting You and the review. **Details view** (owner decision of 2026-10-03: the details go to the bottom, "use native layout mechanisms"): the selected node's details are NOT a column inside the canvas. They are a view of their own, `escurel.details` ("Escurel Details"), a WebviewView contributed to VS Code's **panel** area, so the user docks, moves and resizes it with VS Code's own layout and the canvas keeps the full width. It shows the most recently selected node of any open thread (a click or key on the canvas, or the Threads outline), refreshes when that thread reloads live, and says "Select a node in a thread to see its details." when there is nothing to show or its thread closed. It is brought up without taking focus from the canvas the first time in a session; Esc in it returns the focus to that thread's canvas. Its buttons (Skill split buttons, run controls) go to the host WITH the thread's id, and the host acts only for the thread being shown, through the same validation as the canvas (never an id from the webview). Nodes are clickable: an event opens the thread, a run opens run detail, a draft/changeset opens review, and an instance opens page-as-UI. Inline gate buttons: Promote / Discard. Live via `event_subscribe{root_event_id}`.
6. **Run detail** (webview panel): attempts timeline, harness/model, status, the latest plan (`report_progress` snapshots) with step states, the **trace**: `get_run_tool_calls` as a timeline (tool, ok / failed / rejected in words with the error code, duration in human units, offset from the run start, a bar against the slowest call; each call expands to the size of what it sent and received (the gateway records sizes, not content); paged by `after`), a link to **what the run produced** (`run-finished.produced_instance`; the host opens its own value, never an id from the webview), summary, trace id (copyable). Cancel / Retry. Live via `event_subscribe{run_id}`.
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

Multiple gateways or tenants, offline mode, Marketplace publishing, brand theming, actor swimlane thread views (rows per cascade branch are in scope, see the Thread surface in §3), rendering peacock blocks (render plain markdown tables; defer rich blocks).
