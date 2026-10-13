# Approve plan sent the configured harness without checking the runner's allow-list

**Symptom.** Found by the second crew review. Start (#701) refuses an `escurel.harness` the runner does not
publish, but the plan approval (`escurel.approvePlan`) still put `readConfig().harness` into
`provenance.manual.harness` unchecked, so approving could ask for an engine the runner does not allow.

**Fix.** `approvalHarness` (src/start/chooseHarness.ts) applies the same rule without asking: an allowed or
unknown-list harness is sent, an unlisted one (or a blank setting) sends none and the runner uses its default.
The command reads the runner status before approving.

**Not changed (crew item stale).** `countDelayed` in overview/load.ts is already bounded
(`DELAYED_MAX_PAGES` = 5 pages of 100, a floor flagged as truncated).

## Changelog lines for the release notes
- VS Code: Approve plan no longer sends an `escurel.harness` the runner's allow-list does not name.
