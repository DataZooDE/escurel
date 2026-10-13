# Skill-declared Overview tiles

The Overview board counted records by skill but could not say which shipments were delayed. "Delayed" has no
engine-wide meaning: the status field and the values that count belong to the skill, so a skill declares them.

```yaml
overview:
  tile: Delayed shipments        # plain-words title, <= 60 characters
  status_field: status           # a declared field (or a filterable backend column) of this skill
  delayed_when: [late, delayed]  # the values that count as delayed
  sort_field: days_late          # optional: orders the three lines shown (descending)
```

The engine only carries the declaration. `validate` lints it (`overview_invalid`, `overview_tile_invalid`,
`overview_status_field_unknown`, `overview_delayed_when_invalid`: warnings, never a refusal) and `list_skills`
exposes it additively as `overview` (Rust, TypeScript and Dart types, golden fixture). The extension reads it:
one `list_instances` filter per delayed value, at most 5 pages x 100 in all, counts distinct instances, shows up
to three lines (host-handed open actions, untrusted text through `cleanText`). When the page budget is spent the
count is a floor and the headline reads "500+ delayed". A rows-backed skill whose status field is not filterable
omits the tile (debug log once). No new MCP tool.

The S2D demo overlay does not declare it: its `supplier_exception` records use `status: open`, which would not
honestly count delayed shipments, and `exception_exposure` is a SQL-backed view whose delay is computed in a query.

## Things found while verifying

- The `list_skills` golden is now captured from a real gateway (`scripts/refresh-golden.mjs`) with the
  declaration in the integration seed's `customer-order` skill. The refresh also returned the Evolve built-in
  skills and top-level `evolve_*_revision_binding` keys, which the typed Rust `Skill` / `ListSkillsResponse` do
  not model (the lossless-decode contract test fails on them). The script now drops them from the golden; typing
  them is a separate follow-up.
- No visual baseline changes: the visual harness fixtures do not declare a tile (a real-gateway check of the loader
  against seeded delayed orders returned the expected count and order).

## Changelog lines for the release notes

- Added an optional skill frontmatter `overview` declaration for delayed-instance tiles, with lint-only validation
  and additive `list_skills` wire exposure (skill reference 01 and 02).
- The VS Code Overview board renders skill-declared delayed tiles using bounded filtered reads and host-validated
  instance links; a truncated count reads "N+ delayed".
