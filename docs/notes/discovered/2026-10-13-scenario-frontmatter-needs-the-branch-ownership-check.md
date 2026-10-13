# A hand-written `scenario:` must pass the branch ownership check

**Symptom.** #687 made a branch its author's workspace, but only the `branch:` argument of
`update_page` was checked. Content carrying `scenario: <registered branch>` in its frontmatter
was indexed into that scenario regardless of caller, and `merge_branch` selects by scenario, so
another agent's page would have landed with the owner's merge.

**Fix.** `update_page` without `branch:` now runs `require_open_branch(Write)` when the
frontmatter names a *registered* branch. Unregistered names remain the legacy overlay.

**Not covered.** Other write doors that accept raw frontmatter (drafts, changesets) were not
audited here; check them if branches grow more consumers.

## Changelog lines for the release notes
- `update_page` with a hand-written `scenario:` naming a registered branch now follows branch
  ownership (author/admin only; others get `unknown_branch`).
