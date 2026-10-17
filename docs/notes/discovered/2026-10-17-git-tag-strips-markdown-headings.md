# `git tag -a -m/-F` strips markdown headings, so the release job refused the v2026.10.17 tag

## Symptom

`release.yml` ran green through `gate` and all four builds, then `create GitHub Release` failed in its first step:

```
the CHANGELOG section for v2026.10.17 is BREAKING but the annotated tag message does not say BREAKING
```

No GitHub Release existed afterwards. The tag message, drafted from the CHANGELOG section, did contain `BREAKING`
twice when the file was checked, and the tag was annotated.

## Cause

`git tag` cleans the message with mode `strip` by default: it removes every line that starts with `#`, plus
leading/trailing blank lines. A CHANGELOG section is markdown, and the only places that said `BREAKING` were its
headings (`## v2026.10.17 — BREAKING (...)` and `### BREAKING`). Both lines were silently dropped, leaving 0
occurrences. The `-m "$(sed ...)"` form in the old checklist behaves the same, and `-F file` too. The check
(`grep -c BREAKING` on the draft file) ran on the file, not on the stored tag.

## Fix

Create the tag with `--cleanup=whitespace`, and check the stored message, not the draft:

```sh
scripts/release-tag.sh vYYYY.MM.DD          # local only, verifies BREAKING survived, removes the tag if not
git for-each-ref refs/tags/vYYYY.MM.DD --format='%(contents)' | grep -c BREAKING
```

For v2026.10.17 the tag was deleted locally and on origin, recreated on the same commit with
`--cleanup=whitespace`, and force-pushed (no release existed yet). `scripts/test-release-tag.sh` runs in CI and
pins both the trap (a plain `git tag -m` loses the headings) and the script.

## Recognising it next time

A release run that passes `gate` and the builds and fails only at `create GitHub Release` with the BREAKING
message error. Compare `git for-each-ref refs/tags/<tag> --format='%(contents)'` with the draft: the lines
starting with `#` are missing. The same cleanup affects any tooling that feeds markdown to `git tag`, `git
commit -F` (without `--cleanup=verbatim`) or `git merge -m`.
