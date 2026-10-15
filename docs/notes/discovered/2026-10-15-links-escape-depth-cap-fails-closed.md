# `links_escape` stopped at depth 8 and called the rest "clean"

## Symptom
A directory connector with a `**` glob (`ESCUREL_SQL_FILE_DIRS`) walked at most 8 levels looking for a symlink that leads
out of the operator's directories. A link at level 9 or deeper was never looked at, so the policy check passed: the cap
failed OPEN.

## Cause
`links_escape` returned `false` for a subdirectory it was not allowed to descend into (`depth == 0`). For a fixed-depth
pattern that is right (the glob cannot match below its own depth). For `**` the depth is only a bound, not the reach.

## Fix
For `**` patterns a subdirectory below the cap counts as escaping (nothing looked inside it). Fixed-depth patterns are
unchanged. Test: `credential_policy::tests::a_recursive_glob_refuses_a_tree_deeper_than_the_walk_can_vouch_for`
(red first). Trade-off: a legitimate `**` glob over a tree deeper than 8 directory levels is now refused; use a narrower
glob or an exposed sub-directory.

## Changelog lines for the release notes
- Security: a directory connector glob containing `**` is refused when the tree below it is deeper than the 8 levels the
  symlink check can inspect, instead of passing unchecked.
