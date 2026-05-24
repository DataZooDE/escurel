# Notes

Working tribal knowledge that doesn't belong in the spec or in
inline comments. Organised by purpose, not by chronology.

## Where things live

- [`../../CLAUDE.md`](../../CLAUDE.md) — the engineering working
  contract: eight principles, the PR cycle, locked decisions.
- [`discovered/`](discovered/) — short notes on non-obvious problems
  we ran into and how we fixed them. One file per problem,
  `<YYYY-MM-DD>-<slug>.md`. The goal is to never rediscover the
  same problem twice.

## When to add a `discovered/` note

If you spent more than ~15 minutes on a problem whose cause was
not obvious from the error message or from reading the immediate
code, write a note. Future you (or a future contributor) will
thank you.

Template:

```markdown
# <Problem title>

**Date:** YYYY-MM-DD
**Scope:** <which crate / which subsystem>

## Symptom

What you saw. Exact error message if there was one.

## Cause

What was actually wrong.

## Fix

What you changed. Link to the commit or PR if possible.

## How to recognise next time

The fingerprint to look for so you don't have to debug from
first principles again.
```
