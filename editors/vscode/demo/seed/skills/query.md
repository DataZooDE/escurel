---
kind: skill
id: query
description: An authored, parameterised read over a SQL view. Reports (Peacock) and agents call it with query_instance; adding one is a page write, not a deploy.
---

# query

A query page names its data source (`target:`), declares its typed `params:`, and carries the `sql:`
with `{{target}}` for the view and `:name` for each bound parameter. Callers never send SQL.
