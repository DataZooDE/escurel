---
type: instance
skill: opportunity
id: hoffmann-pilot
opened: 2026-04-02
status: negotiating
customer: [[customer::muenchner-pharma]]
contact: [[contact::hoffmann]]
champion: [[contact::hoffmann]]
economic_buyer: [[contact::hoffmann]]
engagement: [[engagement::hoffmann-spine]]
value_eur: 60000
close_date: 2026-06-30
prev_review: null
identified_pain: clinical-trial units blocked on per-unit Snowflake procurement; want shared platform with tenant isolation
metrics: cut new-tenant provisioning time from 4-6 weeks (procurement-bound) to under 1 day
decision_criteria: tenant write isolation, audit trail, no shared-credential risk, fit with existing Vault setup
decision_process: technical fit signoff (April) → procurement (May) → CTO sign (June)
competition: internal "build on Snowflake organisations" alternative; no external vendor in the room
---

# Münchner Pharma — pilot opportunity

Three-month paid pilot covering the two R&D units most blocked on the
current platform. Goal of the pilot is to prove the tenant-isolation
story is sufficient for clinical-trial data and to land an internal
champion network beyond Hoffmann.

## Where we are (2026-05-24)

- Discovery complete; Hoffmann's architecture team has run an internal
  proof against [[customer::muenchner-pharma]]'s Vault setup.
- Technical signoff arrives end of April — on track.
- Procurement engagement starts mid-May. Hoffmann has lined up the
  procurement lead in advance; expect 4-6 weeks.
- CTO sign-off is the only remaining unknown — Hoffmann is confident
  but the CTO has not personally engaged yet.

## Inherited from [[lead::hoffmann-followup]]

- Budget confirmed (€60k)
- Authority confirmed for pilot
- Need verified through three internal team sponsorships

## Open risks

- CTO has not engaged. If she pushes back on Q2 close, slips to Q3.
- "Build on Snowflake organisations" is the live internal alternative;
  Hoffmann is on our side but the architecture team has not formally
  ruled it out.

## Status

`negotiating` — moving into procurement. Project spawn on close.
