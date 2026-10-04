---
kind: skill
id: supplier-rating
title: Supplier rating
folder: purchasing/suppliers
role: record
tags: [rest, external, portal]
resource: https://procurement-portal.example/api/ratings
description: The procurement portal's rating of a supplier (a REST API outside escurel) - delivery reliability and on-time delivery. The rating can be changed through a reviewed write-back.
autonomy: review
fields:
  - {name: display_name, kind: string, label: "Supplier"}
  - {name: rating, kind: enum, values: [A, B, C], label: "Rating", render: badge}
  - {name: on_time_pct, kind: float, label: "On-time delivery (%)"}
  - {name: region, kind: string, label: "Region"}
backend:
  kind: openapi
  endpoint: ratings_api
  instances: rows
  key: $.id
  linked: true
  writable_columns: [rating]
  list: {path: /ratings, items: $.data, limit_param: limit, cursor: {param: after, from: $.paging.next}}
  read: {path: "/ratings/{id}"}
  write: {method: PATCH, path: "/ratings/{id}"}
  project: {display_name: $.name, rating: $.rating, on_time_pct: $.on_time_pct, region: $.region}
---

# supplier-rating

One instance per supplier of the procurement portal's rating API. The columns are the portal's and
read-only here, except `rating`: a change is proposed, a reviewer approves it, and only then does
escurel change the portal (after checking the supplier has not changed meanwhile).
