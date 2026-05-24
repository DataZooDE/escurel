# Codex `exec review --full-auto` will write unrelated files

**Date:** 2026-05-24
**Scope:** code-review workflow

## Symptom

Ran a periodic code review:

```bash
codex exec review --base 79e5155  # prompt via stdin
```

Codex returned a useful security finding (path traversal in
`escurel-storage`), but `git status` afterwards showed it had
also written a `docs/ux-mock.html` that had nothing to do with
the review. It got staged into the next commit by accident.

## Cause

`codex exec` runs in `--full-auto` mode by default — it has
read **and write** access to the working tree. Even when the
prompt is "review the code, report findings", the model is free
to create or modify files if it judges them helpful.

## Fix

1. Always inspect `git status` after a `codex exec review` run
   before committing.
2. Either:
   - Pass a sandbox flag that disables writes for reviews
     (`codex exec review --sandbox read-only` or the equivalent
     once we confirm the flag name from `codex exec --help`), or
   - Run review in a worktree / branch where unwanted artifacts
     don't bleed into the change you're about to commit.

For Escurel: a code review is purely a read operation. Future
codex reviews should use the most restrictive sandbox available
so artifacts don't end up in PRs.

## How to recognise next time

After any `codex exec` invocation, run `git status`. If files
appear that you didn't intend, they are codex's. Either
incorporate them deliberately or `git restore --staged + rm`
them out of the way.

## Related

- [`../README.md`](../README.md) — the working notes index.
- CLAUDE.md principle on periodic codex reviews (introduced in
  PR 4b) — this discovered note documents an operational pitfall
  the principle should warn about.
