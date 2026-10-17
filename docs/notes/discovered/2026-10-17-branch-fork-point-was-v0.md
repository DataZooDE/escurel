# A branch's fork point was `v0` for every page

**Symptom.** `merge_branch` returned `conflict` ("moved since branch forked at v0 (head is v2) and the edits could not
be auto-merged") for a page the base had not touched since the branch started. A branch tombstone had the opposite
problem: it deleted a page the base had edited after the fork, with no check at all.

**Cause.** `create_branch` recorded `base_version` as `v<max_hlc("")>`. A CRDT version is a PER-PAGE op count and the
page id `''` has no ops, so every branch forked at `v0`. At merge the head of any page with history (`v2`) differs from
`v0`, and the three-way merge replays from version 0. The tombstone path never asked the question.

**Fix.** The branch's first write to a base page (update or delete) records that page's version in `branch_page_bases`
(first write wins); `merge_branch` compares each page against its own fork point and treats a moved tombstone target as
a `conflict`. Pages without a record (created on the branch, or a branch older than the table) use the old
branch-wide value.

**Recognise it.** A `conflict` message that says `forked at v0` while the page has a higher head. Tests:
`crates/escurel-server/tests/suite/branches.rs` (`a_branch_forked_from_a_page_with_history_*`,
`a_branch_tombstone_does_not_silently_delete_*`). Found by the 2026-10-17 crew review.
