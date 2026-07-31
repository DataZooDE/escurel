---
type: instance
skill: decision
id: ship-full-base-model
at: 2026-02-06T09:00:00Z
title: Ship the full-base GBM to production
decided_by: [[stakeholder::marketing-lead]]
motivated_by: [[expectation::churn-v1]]
justified_by: [[result::gbm-auc]]
addresses: [[hypothesis::gradient-boost]]
status: orphaned
---

# Ship the full-base GBM

Made on the strength of [[result::gbm-auc]] under expectation
[[expectation::churn-v1]] (one model for the whole base). That
expectation was **later superseded** by [[expectation::churn-v2]]
(high-value churn only) on 2026-03-02, which is why this decision is now
`orphaned` — it rests on an expectation that no longer holds.
