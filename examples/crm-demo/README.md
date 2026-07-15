# crm-demo

A minimal CRM tenant that exercises every escurel primitive merged to
date: typed frontmatter, typed `[[skill::id]]` wikilinks, the
skill/instance distinction, and a small connected link graph.

The tenant models a sales lifecycle as a chain of **phase-typed
instances** sharing the same conceptual engagement — exactly the
structure the [skill-based CRM brief][brief] describes ("one continuous
instance per real-world entity"). No `engagement_arc` spine skill yet;
the chain is held together via explicit `prev_*` wikilinks on each
instance.

## Skills (type declarations)

| skill | what it declares |
|---|---|
| [`escurel`](skills/escurel.md) | mandatory meta-skill — how the KB is organised |
| [`customer`](skills/customer.md) | a buying entity (company / org) |
| [`contact`](skills/contact.md) | an individual person at a customer |
| [`engagement`](skills/engagement.md) | first-touch interaction |
| [`lead`](skills/lead.md) | qualified follow-up under BANT |
| [`opportunity`](skills/opportunity.md) | named, valued sales motion |
| [`project`](skills/project.md) | delivery engagement post-close |
| [`attachment`](skills/attachment.md) | **document-backed** skill — uploaded files (PDF/DOCX/text), extracted + chunked; read-only |
| [`erp_order`](skills/erp_order.md) | **sql_view-backed** skill — read-only DuckDB view over [`sources/erp/*.json`](sources/erp/); read-only |
| [`stock_quote`](skills/stock_quote.md) | **openapi-backed** skill — live quote proxied from the Yahoo Finance chart API; read-only |

The [`attachment`](skills/attachment.md) skill demonstrates an **external
instance backend** (`backend: kind: document`): rather than authoring a
markdown page, you upload a file to `POST /ingest/upload` and escurel extracts,
chunks, and embeds it into a read-only page-with-chunks. `scripts/verify-demo.sh`
ingests a real PDF through it. See
[the meta-skill](skills/escurel.md#instance-backends) for the backend concept.

## External instance backends (`scripts/demo-setup.sh`)

`ESCUREL_SEED_DIR` only seeds *markdown pages*. External instances are
backend-managed — they must be materialised through the admin tools
against a running server. After booting the demo server, run

```bash
scripts/demo-setup.sh          # ESCUREL_DEMO_BASE overrides the default :8080
```

which (idempotently):

1. **sql_view** — resolves the `erp_order` skill's repo-relative
   `source.relation` to the absolute [`sources/erp/`](sources/erp/) path
   (DuckDB resolves the `json_dir` glob against the server cwd) and
   materialises `[[erp_order::book]]` via `create_sql_instance`.
   `expand` on that instance then returns a bounded row projection with
   the projected columns mirrored under the `source.<field>` namespace —
   fully offline.
2. **openapi** — registers the `yahoo_finance` endpoint (default
   `https://query1.finance.yahoo.com`; override with
   `ESCUREL_DEMO_YAHOO_BASE`) and materialises `[[stock_quote::sap]]`
   via `create_remote_instance`. Note escurel never fetches the OpenAPI
   document: `register_endpoint` takes only name/kind/base-URL/auth, and
   the per-operation binding (read path + JSONPath projection over
   `chart.result[0].meta`) lives on the
   [`stock_quote`](skills/stock_quote.md) skill page. The
   [`sources/yahoo-finance-openapi.json`](sources/yahoo-finance-openapi.json)
   spec documents that operation for humans/agents.

   **Live quotes require internet.** Offline, `expand` on the quote
   instance shows the documented fail-closed degraded path — a
   `backend_projection.issue`, never a fabricated price — and
   `validate_endpoints` reports the endpoint `unreachable`. That
   degraded path is itself part of the demo.

The no-mock acceptance for both flows lives in
`crates/escurel-server/tests/crm_demo_backends.rs` (the openapi tests
run against a real local Yahoo-shaped HTTP server; the internet is
never touched by tests).

## Instances — the Hoffmann chain (Brief scenario A)

```
customer::muenchner-pharma
        ↑
contact::hoffmann
        ↑
engagement::hoffmann-intro
        ↓ follow_up
lead::hoffmann-followup
        ↓ prev_event
opportunity::hoffmann-pilot
        ↓ (project::* spawns at commercial close — not yet seeded)
```

| instance | role in the chain |
|---|---|
| [`customer__muenchner-pharma`](instances/customer__muenchner-pharma.md) | the buying organisation |
| [`contact__hoffmann`](instances/contact__hoffmann.md) | Dr. Andreas Hoffmann at MP |
| [`engagement__hoffmann-intro`](instances/engagement__hoffmann-intro.md) | summit booth conversation, 2026-03-12 |
| [`lead__hoffmann-followup`](instances/lead__hoffmann-followup.md) | qualified lead, status open |
| [`opportunity__hoffmann-pilot`](instances/opportunity__hoffmann-pilot.md) | three-month pilot, €60k, mid-stage |

Scenarios B (PharmCo Schweiz) and C (Müller Maschinenbau) from the
brief layer in once the `escurel-explore` editor's scrubber surface
calls for them — likely in a later PR once live mode lands.

## Loading this tenant

- Today: the `FixtureEscurelClient` in `apps/escurel-explore/` reads
  these files via `rootBundle` (PR-3 wires that up).
- M3+: `escurel admin seed examples/crm-demo/` uploads every page
  into a live LaneStore.

## Conventions

- Skill file names mirror the skill id: `engagement.md` → `id: engagement`.
- Instance file names are `<skill>__<id>.md` (double underscore separator)
  to keep them sortable and grep-friendly.
- Wikilinks use the typed form `[[skill::id]]` everywhere except the
  meta-skill's narrative prose.
- Required frontmatter fields are declared on each skill page and
  validated by the `validate()` tool when M3 lands.

[brief]: ../../docs/notes/
