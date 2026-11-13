# Flutter web: a leaf semantics node's label is text content, not aria-label

**Symptom.** `scripts/verify-demo.sh` failed `region semantics not found:
page-cost-badge` although the badge was on screen and the widget test found
it with `find.bySemanticsLabel`. The script's presence check counted
`flt-semantics[aria-label*="…"]`.

**Cause.** Flutter web (3.44) renders a semantics node's label as
`aria-label` only when the node has semantics children (a container). A
LEAF node — e.g. `Semantics(label:, value:, excludeSemantics: true)` around
a badge — gets its label (plus value) as the element's text content and no
`aria-label` at all. The existing `tab-*` / `*-panel` labels are all
containers, so the selector had only ever met containers.

**Fix.** `label_count` in `scripts/verify-demo.sh` matches a node when its
`aria-label` contains the token OR it is a leaf (no `flt-semantics`
descendant) whose text contains it; `leaf_text` reads such a node's
label + value (`page-cost-badge $0.42 · 2 runs (30d)`).

**Recognise it next time.** A label the widget test finds but rodney's
`aria-label` query does not, on a node with `excludeSemantics: true` or no
semantic children. Dump
`Array.from(document.querySelectorAll('flt-semantics')).map(e => e.getAttribute('aria-label') + '#' + e.textContent)`
and look for the token after the `#`.
