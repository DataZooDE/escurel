# `.gitignore` does not support inline comments

**Date:** 2026-05-24
**Scope:** repo bootstrap

## Symptom

`Cargo.lock` was appearing in `git status` as an untracked file
despite this entry in `.gitignore`:

```
Cargo.lock           # mixed bin+lib workspace; revisit when first binary lands
```

`git check-ignore -v Cargo.lock` returned no output (i.e. the file
was NOT being ignored).

## Cause

`gitignore` syntax only treats `#` as a comment marker when it
appears at column 0 (start of line). On a pattern line, the `#` and
everything after it — including the leading whitespace — become part
of the pattern. So the rule actually being applied was the literal
pattern `Cargo.lock           # mixed bin+lib workspace; revisit
when first binary lands`, which matches nothing.

Confirmed in `man gitignore`:

> A line starting with `#` serves as a comment. Put a backslash
> (`\`) in front of the first hash for patterns that begin with a
> hash.

(Note the wording: "starting with". Mid-line `#` is *not* a comment.)

## Fix

Move the comment to its own line above the pattern:

```
# Library-only workspace today; revisit when first binary lands.
Cargo.lock
```

After this change, `git check-ignore -v Cargo.lock` correctly
reports the file as ignored by `.gitignore:7`.

## How to recognise next time

If a `.gitignore` rule looks correct but the file still appears in
`git status`, run `git check-ignore -v <path>` first — it will
report which rule matched, or no output if nothing matched. The
most common cause of "nothing matched" on a syntactically-plausible
rule is an inline `#` comment turning the whole line into a
nonsense pattern.

The same trap exists in `.dockerignore`. Different tool, same
gitignore-style syntax, same gotcha.
