# Branch decisions are author-or-admin (2026-10-13)

**Symptom / rule.** After #687 ("a branch is its author's workspace") any *person* could still
`merge_branch` or `abandon_branch` someone else's branch, so one person could land or destroy another's
work. Owner decision 2026-10-13: only the branch's author or an admin may decide it.

**Behaviour.** `require_open_branch` now has one rule for write, merge and abandon: `is_admin ||
branch.author == caller.subject`. Anyone else gets `unknown_branch`, identical to a missing name (no
existence oracle). The `BranchUse` purpose parameter is gone.

**Machine branches.** A workbench agent token (`mint_agent_token`) is verified with `subject` = the
minting human (`act.sub` becomes the actor), so a branch the machine opens is authored by that human:
they can merge or abandon it, as can an admin. Another person, or another person's machine, cannot. A
machine still cannot merge a branch that touches a review/skill page (`refuse_machine_removal`).
Runner-minted tokens keep their own subject and are admin, so admins still review them.

**Recognise it next time.** A "branch not found" for a branch you know exists means you are neither its
author nor an admin.

Tests: `branches.rs::a_branch_belongs_to_its_author_for_writes_and_decisions`,
`a_machines_branch_belongs_to_the_person_it_acts_for`.

## Changelog lines for the release notes

- `merge_branch` / `abandon_branch`: only the branch's author or an admin may do it; any other caller
  (including another person) gets `unknown_branch`. Supersedes #687's "a person may decide a machine's
  branch". Skill 02-tool-surface updated.
- Fold into `docs/notes/breaking-wire-changes.md` line 126: replace "merge/abandon also allow a person (not
  another machine)" with "merge/abandon are author-or-admin too".
