---
type: instance
skill: engagement
id: hoffmann-spine
at: 2026-03-12T14:20:00Z
with: [[contact::hoffmann]]
channel: event
spine: true
template: engagement_skill v1
phase: delivering
commercial_model: fixed-price base + T&M change pool
contract_value: €350k base + €90k T&M pool
running_margin: 0.66
risk_surface: 0.40
sentiment_trend: 0.86 · positive · monotonic
primary_sponsor: [[contact::reiter]]
champion: [[contact::weber]]
orgunit: [[customer::muenchner-pharma]]
customer: [[customer::muenchner-pharma]]
---

# Engagement — Münchner Pharma S&OP arc

The **continuous spine** for Münchner Pharma — created at first
qualification (T+0, the booth conversation with [[contact::hoffmann]])
and persisting through sales close, delivery, and renewal. One skill
instance for the whole lifecycle, not a copy-forward chain of
Lead → Opportunity → Quote → Contract → Project.

## Lineage

- [[lead::hoffmann-followup]] — sales-phase · BANT · terminated *qualified*
- [[opportunity::hoffmann-pilot]] — sales-phase · MEDDPICC · terminated *won*
- [[project::hoffmann-pilot]] — delivery-phase · fixed-price · *delivering*
- [[change_order::hoffmann-3site]] — scope change · 3-site data model · *accepted*
- [[renewal::hoffmann-2027]] — renewal cycle · *qualifying-with-priors*

## Inherited commitments (sales → delivery obligations)

Promises made during the sale attach to **this engagement** and
become delivery obligations by construction — not to an archived
opportunity row.

- "tenant write isolation sufficient for clinical-trial data" — origin: discovery call → fulfilled
- "new-tenant provisioning under 1 day" — origin: proposal → on track
- "reference-customer case study, low engineering burden" — origin: proposal → fulfilled

## Buying group

- [[contact::reiter]] — CIO · economic buyer → executive sponsor
- [[contact::weber]] — Head of R&D Platform · champion
- [[contact::hoffmann]] — VP Engineering · first contact, technical authority
