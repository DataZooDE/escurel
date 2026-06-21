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

The [`attachment`](skills/attachment.md) skill demonstrates an **external
instance backend** (`backend: kind: document`): rather than authoring a
markdown page, you upload a file to `POST /ingest/upload` and escurel extracts,
chunks, and embeds it into a read-only page-with-chunks. `scripts/verify-demo.sh`
ingests a real PDF through it. See
[the meta-skill](skills/escurel.md#instance-backends) for the backend concept.

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
