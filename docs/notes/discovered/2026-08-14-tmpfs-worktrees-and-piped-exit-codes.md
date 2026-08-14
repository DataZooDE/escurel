# Worktrees on tmpfs, and pipes that eat exit codes

Two unrelated traps from the 2026-08-14 API-review PR series
(#383–#395), both of the "nothing failed loudly" kind.

## 1. Agent worktrees on /tmp filled the 63 GB tmpfs in one afternoon

**Symptom.** Mid-build, every background command started dying with
ENOSPC — including the harness's own task-output writes, so the
*diagnostics* vanished along with the builds. `df /tmp`: 100% of 63 GB.

**Cause.** Five `git worktree add` checkouts under `/tmp/wt-escurel/`,
each accumulating a 13–16 GB `target/` during its four-command gate.
tmpfs is RAM+swap-backed and much smaller than the disk; the CLAUDE.md
disk-discipline section assumes worktrees live on the real filesystem.

**Fix / recognition.** Put worktrees on disk (`/home/jr/wt-escurel/`).
`git worktree move` cannot cross filesystems ("Invalid cross-device
link") — instead: delete `target/` (regenerable), `cp -a` the tree,
`rm -rf` the old one, then `git worktree repair <new-path>` (the
"gitdir incorrect" lines are it fixing the links, not failing).
Recognise the trap early: `df -h /tmp` before a fan-out, and treat any
sudden "output was lost / write error" from unrelated tools as a
possible full-tmpfs, not N independent bugs.

## 2. A pipeline's exit status is the LAST command's — gates lied green

**Symptom.** A local four-command gate printed `GATE_OK`, CI then
failed the same commit on a clippy error (`too_many_arguments`) that
local clippy *had* reported — into a pipe.

**Cause.** `cargo clippy --workspace --all-targets -- -D warnings 2>&1
| tail -1` — the pipeline's status is `tail`'s (0), so `&&` sails past
a failing clippy. Same family: `cargo test | grep -E "FAILED"` shows
failures but never fails the chain, and `grep -c pattern file` exits 1
when the count is 0, silently breaking `&& push` chains in the OTHER
direction.

**Fix / recognition.** Never gate on a piped command without
`set -o pipefail`; better, keep the status observable:
`cargo clippy ... >/dev/null 2>/tmp/clippy.log; echo "clippy: $?"`.
For counts, `grep -c || true` — or test the value, not the exit.
Recognise it whenever "local gate green, CI red on the same check":
suspect the plumbing before the toolchain.
