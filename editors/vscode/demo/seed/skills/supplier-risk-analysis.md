---
type: skill
id: supplier-risk-analysis
description: The persisted result of one supplier-risk run - what was found about a supplier, the orders it puts at risk, how much, and what to do about it.
autonomy: review
required_frontmatter: [supplier, risk_level, risk_score, orders_affected, net_value_at_risk, currency]
fields:
  - {name: supplier, kind: link, target_skill: supplier, required: true, label: "Supplier"}
  - {name: vendor, kind: int, label: "Vendor (LIFNR)"}
  - {name: material, kind: string, label: "Material (MATNR)"}
  - {name: risk_level, kind: enum, values: [low, medium, high], required: true, label: "Risk level", render: badge}
  - {name: risk_score, kind: int, min: 0, max: 100, required: true, label: "Risk score (0-100)"}
  - {name: days_moved, kind: int, min: 0, label: "Confirmation moved (days)"}
  - {name: orders_affected, kind: int, min: 0, required: true, label: "Orders affected"}
  - {name: net_value_at_risk, kind: float, min: 0, required: true, label: "Net value at risk", render: money}
  - {name: currency, kind: string, required: true, label: "Currency"}
actions:
  - name: notify-customer
    kind: event
    label: Notify the affected customers
    event: customer-notice
    title: "{id}"
    body: "Tell the customers of the orders in {id} that delivery is at risk."
  - name: request-confirmation
    kind: event
    label: Ask the supplier for a new confirmation
    event: confirmation-request
    title: "{id}"
    body: "Ask the supplier of {id} for a binding confirmation date."
  - name: ask-why
    kind: prompt
    label: Why is this risky?
    prompt: "Why is {id} rated {frontmatter.risk_level}, and what would lower the risk?"
viewer: { report: supplier-risk-report, param: analysis }
---

# supplier-risk-analysis

One instance per supplier-risk run: what the run worked out, kept as knowledge instead of
vanishing with the run. The fields are the result in numbers; the body is the result in words.

**Every chart has a text alternative.** A graph is drawn by the viewer (`supplier-risk-report`,
rendered by Peacock), but the body of each analysis also states the takeaway of that chart as one
plain sentence and carries the table the chart is drawn from, so an agent or a reader that only
sees this markdown learns the same thing.

The analysis is written by the run into the SAME changeset as its change to the order, so one
promotion publishes both and the order links to its analysis.
