---
type: instance
skill: engagement
id: hoffmann-intro
at: 2026-03-12T14:20:00Z
with: [[contact::hoffmann]]
channel: event
outcome: warm
follow_up: [[lead::hoffmann-followup]]
---

# Hoffmann — Datazoo summit intro

Twenty-minute conversation at the booth on the second day of the
Datazoo summit in Berlin. Dr. Hoffmann came specifically looking for
tenant-isolation patterns that don't require a fresh Snowflake
account per consuming team.

## What we covered

- Walked him through the [[customer::muenchner-pharma]] use case
  (clinical-trial units wanting their own analytical sandboxes
  without procurement overhead).
- Showed the LaneStore design with per-tenant DuckDB write isolation
  and the typed-skill model on top.
- He pushed hard on cross-tenant federation — I explained federation
  is a layer above the per-tenant server, scoped to a separate v2
  surface.

## Outcome

Left with a printed copy of `docs/spec/platform.md`. Asked for a
follow-up call once he'd had a chance to read it. Promised a reply
"by end of next week."

## Follow-up

Spawned [[lead::hoffmann-followup]] on 2026-03-15 after he replied.
