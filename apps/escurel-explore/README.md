# escurel-explore

A general Flutter web editor on top of escurel. Tracks every escurel
capability as it lands — when the server gains a feature, the editor
gains an affordance.

This is *not* a problem-specific UI (those are [heron][heron] and
[herkules-ui][herkules]). It is the canonical client that dogfoods the
12-tool agent contract plus admin MCP tools, and is deployed tailnet-only
on the substrate so the team always sees what `main` does.

## Run locally (fixture mode)

```bash
flutter pub get
flutter run -d chrome --dart-define=ESCUREL_EXPLORE_MODE=fixture
```

## Run against a real server (M3+)

```bash
flutter run -d chrome \
  --dart-define=ESCUREL_EXPLORE_MODE=http \
  --dart-define=ESCUREL_EXPLORE_BASE_URL=http://dz-escurel.nonprod.int:8080
```

## Test

```bash
flutter analyze
flutter test
flutter test integration_test/ -d chrome --web-renderer canvaskit
```

## Layout

Three-pane workspace: catalogue (left) / entity editor (center) / right rail
(backlinks + outgoing links + neighbours graph). Topbar holds the tenant
selector, global search (lit up when M2 lands), and the backend status chip.
Bottom write surface routes to `validate` / `update_page` / live-mode
`apply_op` as those tools land.

The right rail's **Chat** tab is the governed chat agent: a question is
appended to a private conversation (a `conversation` instance from the chat
corpus, `examples/chat/seed`) and answered by an `escurel-runner` run as the
person who asked; the tab polls `list_messages` until the answer arrives.
It needs `whoami` (to name the person's page chat) and, for answers, a
gateway that verifies tokens plus a minted-mode runner — see
`scripts/verify-chat.sh`. `?rail=chat` opens the rail on the tab (also
`links|events|webhooks|members`).

Run usage and cost (`usage_report`, admin-only) shows in two places: the
page header carries a **cost badge** — what the runs that targeted the page
cost over the last 30 days (`$0.42 · 7 runs (30d)`), hidden for a non-admin
(`whoami.is_admin`) or a page no run touched — and the Dev Inspector's
**Agents** panel (`/inspector/agents`) lists the tenant's agent profiles
(`agent` instances: harness, model, scopes) with each one's runs, success
rate and cost. A skill that declares `schedule:` shows its cron (and
`schedule_budget` cap) in the catalogue and the skills registry.
The **Runtimes** panel (`/inspector/runtimes`) lists the registered runners
(`list_runtimes`): online, stale, stopped or not reporting (in words and by
icon shape), the harnesses each offers (with CLI versions for an admin), its
labels, live runs and when it was last seen.

[heron]: https://github.com/DataZooDE/heron
[herkules]: https://github.com/DataZooDE/herkules-ui
