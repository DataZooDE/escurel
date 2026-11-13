# A view's title actions show only on hover once its container holds several views

**Symptom.** After the Usage and Agents views joined Runs and Chat in the `escurel-runner`
container, the live e2e scenario "the Runs panel is a control center" timed out on
`page.getByRole('button', { name: /Filter runs/ })`. The button exists; it is not rendered.

**Why.** A view alone in its container puts its title actions in the container's title bar,
always visible. With several views each view has its own pane header, and VS Code draws that
header's actions only while the pane is hovered or focused. Whether a test passed depended on
where an earlier step had left the mouse.

**Fix.** Hover the pane header, then click the action inside it: `runsAction(page, name)` in
`editors/vscode/test/e2e/helpers.ts`. Use it for every Runs title action.

**Recognise it next time.** A title-action button "not found" right after a view was added to
(or moved into) a container that already held one.

## Related: the hex-colour lint reads issue numbers

`scripts/lint-hex.mjs` forbids literal colours in `webview/`, and `#820` or `#822` in a COMMENT
is three hex digits to it. Refer to issues by name in webview sources, or not at all.
