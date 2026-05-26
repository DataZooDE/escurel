# 07 — Fixtures and seeding

To test your app you need a tenant with content. Seeding always goes
through the **public `update_page` write path** — never side-doors the
indexer. The guarantee (`docs/spec/dx.md` §Fixture/seeding façade): *what
you seed is exactly what `update_page` would seed in production.* This
keeps fixtures honest as the indexer evolves.

## Authoring seed pages

Seed pages are ordinary skill/instance markdown (`references/01`). Keep
them as files and pull them in with `include_str!` so they double as
documentation. Live examples: `examples/echo-app/tests/fixtures/` and
`examples/crm-demo/`.

A skill:

```markdown
---
type: skill
id: customer
description: A buying organisation tracked by the sales team.
---

# customer

The `customer` skill represents a buying organisation. …
```

An instance:

```markdown
---
type: instance
skill: customer
id: acme-corp
---

# Acme Corp

Acme Corp is a long-standing customer …
```

Page ids are repository-relative paths:
`markdown/skills/<id>.md` for skills, `markdown/instances/<skill>/<id>.md`
for instances. The builder derives these for you.

## Three seeding routes (same write path under all of them)

1. **`FixtureBuilder` (Rust tests).** The ergonomic route; runs inside
   `EscurelProcess::spawn` before it returns:

   ```rust
   FixtureBuilder::new()
       .tenant("acme")
           .skill("customer",  include_str!("fixtures/customer.skill.md"))
           .instance("customer", "acme-corp", include_str!("fixtures/acme-corp.md"))
           .page("notes/error-catalogue.md", include_str!("fixtures/errors.md")) // escape hatch
           .done()                       // back to FixtureBuilder; chain more tenants
   ```
   - `.skill(id, body)`, `.instance(skill, id, body)`, `.page(path, body)`,
     `.done()`; `body` is `impl Into<MarkdownBody>` (`&str`, `String`,
     `include_str!`).
   - `.page(...)` is the escape hatch for anything the structured helpers
     don't cover (a bare `note`, the `[[error-catalogue]]` page, etc.); a
     leading `markdown/` is optional.
   - Seeding failure (bad frontmatter, missing required key) **panics**
     `spawn` — the right behaviour for a fixture.

2. **CLI `update-page` (any language / scripts).** Loop over your files:

   ```sh
   for f in fixtures/skills/*.md;            do escurel update-page "markdown/skills/$(basename "$f")"            < "$f"; done
   for f in fixtures/instances/customer/*.md; do escurel update-page "markdown/instances/customer/$(basename "$f")" < "$f"; done
   ```

3. **`update_page` over the wire (any language).** POST the `update_page`
   tool over `/mcp` (`references/03`), or call `Client::update_page`
   (`references/05`), once per page.

## The mandatory `escurel` meta-skill

Production tenants are created with the `escurel` meta-skill page already
present (`platform.md` §tenant Create: "drop in the `escurel` meta-skill
page"). In tests you start from an empty tenant, so if your test drives a
runtime **agent** that loads the meta-skill, seed it too (copy
`examples/crm-demo/skills/escurel.md` and append tenant-specific guidance).
If your test only exercises your backend's typed calls, you don't need it.

## Tips

- Seed the **minimum** that makes the behaviour observable; the
  `unknown_*` cases want an *empty* tenant on purpose.
- Order doesn't matter for validation of required keys, but a wikilink to a
  not-yet-seeded page resolves as `exists: false` — seed targets you assert
  reachability on.
- Keep fixtures in `tests/fixtures/` and `include_str!` them; this is what
  `examples/echo-app` does and it keeps the test readable.
