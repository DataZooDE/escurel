# A derived OR made the event-pane chevron dead on skill pages

**Symptom.** In the escurel-explore CRM workspace, once the left event
pane was collapsed it could not be re-opened — on a **skill** page the
expand chevron did nothing at all. (On instance pages collapse/expand
worked fine, which is why it looked intermittent.)

**Cause.** The event pane auto-minimizes on skill pages (skills carry no
events). That was modelled as a derived OR:

```dart
final effectiveLeftCollapsedProvider = Provider<bool>((ref) =>
    ref.watch(leftCollapsedProvider) || ref.watch(currentPageIsSkillProvider));
```

The chevron wrote `leftCollapsedProvider`. On a skill page
`currentPageIsSkill` is `true`, so the OR is stuck `true` no matter what
the chevron writes — the toggle is **dead**. Clicking expand set
`leftCollapsedProvider = false`, but `false || true` is still `true`.

**Fix.** Make `leftCollapsedProvider` an *explicit choice* (`bool?`,
`null` = follow the page-type default) and have `effective` prefer it
over the skill default:

```dart
final leftCollapsedProvider = StateProvider<bool?>((ref) => null);
final effectiveLeftCollapsedProvider = Provider<bool>((ref) {
  final choice = ref.watch(leftCollapsedProvider);
  return choice ?? ref.watch(currentPageIsSkillProvider);
});
```

`_SplitBody` resets the choice to `null` when the focused page flips
skill↔instance (`ref.listen(currentPageIsSkillProvider, …)`), so each
context falls back to its sensible default (skills minimize, instances
show events) while the chevron is always authoritative for the current
page.

**How to recognise it.** A user-toggleable boolean that is OR-ed (or
AND-ed) with a derived condition in its *effective* value. The derived
term silently wins, so the control appears inert in exactly the state
where the derived term is active. If a toggle "does nothing" only in a
particular context, look for a derived value masking the user's flag —
prefer "explicit choice overrides default" (nullable override) over
"flag OR condition."

**Test.** `crm_workspace_test.dart` →
`the event-pane chevron re-opens an auto-minimized skill page` drives the
real chevron on a skill page and asserts it expands. Verified red under
the old OR-logic, green after the fix.
