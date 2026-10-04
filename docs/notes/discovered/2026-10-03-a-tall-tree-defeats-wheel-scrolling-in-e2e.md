# Live e2e: a taller Knowledge tree broke "scroll to the row"

**Symptom.** Adding two skills to the demo made the (unrelated) story test fail: `folder plumbing` "not
found", with a screenshot showing the tree scrolled to the bottom. The tree was fine; the gateway listed
every skill.

**Cause.** The tree is virtualised and tiny, and the test helper scrolled to the top with a mouse wheel
and immediately walked down. The wheel scroll is animated; with more rows the animation lost the race
against the downward steps, so the helper walked past the first rows.

**Fix.** `knowledgeRow` in `test/e2e/live.spec.ts` focuses the list and presses Home (the list's own
keyboard handling), then scans. Separately: right after the window opens, live runner events refresh the
tree and collapse what a test just expanded, so `openRow` re-expands and retries rather than trusting one
click. And a source that is down cannot be navigated to through the tree (it has nothing to list), so a
test that needs the page of a dead source re-activates its already-open tab.
