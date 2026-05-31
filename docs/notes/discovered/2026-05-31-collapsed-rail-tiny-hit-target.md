# A collapsed pane's expand chevron was a too-small hit target

**Symptom.** On the CRM workspace, collapsing a pane worked, but the
**expand** chevron in the resulting rail often did nothing for a real
mouse — "I can collapse, but afterwards expand is not possible." Worst
when *both* panes were collapsed (a near-blank screen with two tiny
chevrons that wouldn't respond).

**Cause.** The collapsed rail rendered `Center(child: toggle)` where
`toggle` was a 16px `IconButton`. Only that ~16–28px icon was
tappable. The collapse button, by contrast, lives in a full-width 28px
header bar (easy to hit) — hence "collapse works, expand doesn't." When
both panes collapse, the *right* rail also balloons to most of the
width (`leftW = rail`, `rightW = w - rail - divW`), so the chevron sits
alone in a large blank region that ignores clicks everywhere except the
icon.

**Why tests missed it.** `tester.tap(find.bySemanticsLabel(...))` taps
the widget's **center**, which is exactly where the icon is — so every
collapse/expand test passed while real off-icon clicks failed. The
regression test now uses `tester.tapAt(...)` at an offset *away* from the
centered icon (near the top of the rail) to exercise the real hit area.

**Fix.** Make the whole collapsed rail the tap target: wrap the rail in
an `InkWell(onTap: onToggle)` filling the region, with the chevron
centered for affordance. A tap anywhere in the rail re-expands it.

**How to recognise it.** A control that "doesn't work" for real users
but passes every widget test. Check whether the tappable area is a small
centered child while the test taps dead-center — `tester.tap` hits the
center regardless of size, so it cannot catch an undersized hit target.
Use `tapAt` with an offset, and prefer generous, full-region hit targets
for rails/toggles.
