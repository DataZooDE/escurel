# Closing an overlay before an awaited nav disposes its `ref`

**Symptom.** In the escurel-explore demo, clicking a skill in the ☰
skills registry did nothing on the *live* (HTTP) gateway — the menu
closed but the skill page never opened. The browser console showed an
opaque release-mode `Uncaught Error`. Every widget test passed, so the
regression was invisible to the suite.

**Cause.** `_SkillsPanel._open` (the skill-row tap handler) was:

```dart
void _open(WidgetRef ref, String skillId) {
  close();                      // tears down the OverlayPortal NOW
  focusSkill(ref, skillId);     // async: awaits a `resolve` round-trip
}
```

`focusSkill` is `async` — it `await`s a real `resolve` HTTP call, then
calls `navigateToInstance(ref, …)`. But `close()` runs first and
disposes the overlay that owns `ref`. By the time the awaited `resolve`
returns, the `_SkillsPanel` element is defunct, so the post-await
`ref.read(...)` inside `navigateToInstance` hits a disposed element and
throws (release mode reports it only as `Uncaught Error`). Navigation
never happens.

With the **fixture** client `resolve` completes in a microtask, so on a
fast path the order rarely bit; the real gateway's network latency made
it deterministic.

**Fix.** Await the navigation *before* closing the menu, so the
overlay's `ref` is still mounted when `navigateToInstance` runs:

```dart
Future<void> _open(WidgetRef ref, String skillId) async {
  await focusSkill(ref, skillId);
  close();
}
```

**How to recognise it.** Any overlay/menu/dialog tap handler that
`close()`s the transient route *and then* `await`s something using the
same `WidgetRef`. The shape is "close, then await, then use ref." If the
awaited work needs the ref, await first and close last (or capture a
stable container before closing).

**Why no widget test catches it.** `flutter_test` keeps the
`ProviderContainer` alive for the whole test, so a `WidgetRef.read`
after the consumer unmounts still resolves instead of throwing — the
real Flutter engine deactivates the element and throws. A test with the
buggy ordering (even with injected `resolve` latency) still passes. We
verified this directly: reverting the fix leaves the suite green. The
real safety net here is the no-mock browser check
(`scripts/verify-demo.sh` / rodney), not a unit test. Don't add a
widget test that claims to guard this ordering — it gives false
confidence.
