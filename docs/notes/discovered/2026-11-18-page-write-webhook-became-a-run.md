# A `page_write` webhook notification became a run

**Symptom.** heron-escurel, right after moving to v2026.11.17 (2026-11-18). The runner logged this chain:

1. `claim_event refused its arguments` (`` `event_id` is required ``);
2. `webhook trigger admitted without a routing/claim answer`;
3. `dispatch: packaging failed` (`label_skill "runtime" did not resolve to a skill page`);
4. a dead-lettered run.

Every `event_id` in those lines was empty.

**Cause.**
- heron-escurel sets `ESCUREL_WEBHOOK_URL` to the runner's `/trigger`. The gateway posts there for every captured event, and also for every confirmed instance write: `emit_page_event` sends `{kind: page_write, label_skill, instance_page_id, new_version}`, as a notification, not work.
- `/trigger` parses any body as an `escurel_types::Event`. The struct is `#[serde(default)]`, so the notification parsed fine, with an empty `event_id`, `kind: page_write`, and the page's skill as the label.
- The run then failed on the empty id:
  - the claim refused it;
  - a lone runner holds an undecidable webhook on its ledger;
  - the packager looked for the skill.
- It surfaced only now because v2026.11.17 runners write their own `runtime` page (#818), so every runner start makes an instance write.
- Older runners also enqueued these notifications. Their ledger deduplicated on the empty id, so at most one bogus run per ledger appeared, and nobody noticed.
- The lab's dz-escurel does not point its webhook at a runner, so it never showed this.

**Fix.** `/trigger` acknowledges a body without an `event_id` with 202 and takes nothing. Test: `crates/escurel-runner/tests/suite/page_write_webhook.rs`. It uses a real gateway webhook, a real runner and an instance write, checks the delivery log, and runs a captured event as positive control.

**Recognise it next time.** Runner warnings with `event_id: ""`, or a dead-lettered run for a skill named after a page path segment (`runtime`, or any instance skill), on a deployment whose gateway has `ESCUREL_WEBHOOK_URL` set.
