# Scenario overlays: the QUALIFY override + what raw queries still see

## The model

Scenarios A/B/C are a nullable `scenario` column on `pages` + `blocks`
(`sql/0003_scenarios.sql`), mirrored from frontmatter `scenario:` in
`update_page`. `NULL` = the shared base timeline; a value = a what-if
overlay that **adds or overrides** base pages but never tombstones them.

A base page and its overlay are two **different files** (different
`page_id`s) sharing the same **slug** (`frontmatter.id`).

## The override (and the trap)

Reads take an optional `scenario`:

- `None` → `WHERE scenario IS NULL` (base only — overlays are invisible).
- `Some("B")` → `WHERE (scenario = ? OR scenario IS NULL)` plus a per-slug
  dedup so the overlay wins over its base twin:

  ```sql
  QUALIFY ROW_NUMBER() OVER (PARTITION BY slug ORDER BY scenario NULLS LAST, page_id) = 1
  ```

  `ORDER BY scenario NULLS LAST` is the crux: the non-null (overlay) row
  sorts first → row 1 → kept; the base twin is dropped. Flip the NULLS
  ordering and you'd keep base and silently ignore the overlay — the
  feature would look broken in a way no type error catches. The `,
  page_id` tiebreaker keeps the pick deterministic.

`resolve` mirrors this with `ORDER BY scenario NULLS LAST LIMIT 1`.

## What scenarios do NOT touch

- **Raw stored queries** (`run_stored_query`) are scenario-agnostic: the
  user's SQL runs against the whole `pages` table, so it counts/returns
  overlay rows too. A test that counts instances of a skill must expect
  base + overlay rows once any overlay is seeded (this bit the
  `run_stored_query` count test when a scenario-B fixture was added).
- **neighbours / search / expand** do not yet take a scenario in the
  first cut (PR-9 wired `list_instances` + `resolve`, which the demo's
  scenario switch needs). Threading the rest is a mechanical follow-up.

## How to recognise it next time

If a scenario switch "does nothing" or shows base values, check the
`NULLS LAST` direction in the QUALIFY/ORDER BY first. If a count is
unexpectedly high, remember raw SQL sees every scenario row.
