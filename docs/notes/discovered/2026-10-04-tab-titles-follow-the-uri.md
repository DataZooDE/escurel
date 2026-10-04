# Editor tab titles follow the URI, so instance tabs read `skill__id.md`

**Symptom.** An instance opened in Escurel Page shows a tab named `customer-order__order-4500131.md`.
Users who know orders, not escurel, read that as a file name and cannot tell two tabs apart at a glance.

**Why.** A custom editor's tab label is the basename of the resource URI, and VS Code offers no API to
override it for a `CustomReadonlyEditorProvider` (only a `WebviewPanel` created by the extension has a
settable `title`). The `escurel:` URI mirrors the page id (`markdown/instances/customer-order__order-4500131.md`
becomes `escurel:/instances/customer-order__order-4500131.md`), and `src/fs/read.ts` (`pageIdFromPath`) maps it
back. Shipped corpora use the `skill__id.md` layout; the id is the identity, rows pages and write-back
witnesses key on it.

**What was considered.** Nesting the URI as `/instances/<skill>/<id>.md` would give the tab `<id>.md`, but
`pageIdFromPath` would then return `markdown/instances/<skill>/<id>.md`, which is a different page id from
the real one. Every read, stat and save would need a reverse translation table, and a corpus that really
uses nested ids would become ambiguous. That is a data-identity risk for a cosmetic gain, so it was not done.

**What we do instead.** The names are in the page. The Page-as-UI header shows the record's title (`<h1>`),
the skill link and the thread strip; review diffs name the skill in the tab (`<skill> · <page> — draft by …`,
which we control because those are `vscode.diff` titles); the thread canvas and Details panel use titles.
If VS Code ever lets a custom editor set its label, set it to the record title and the skill.

**Recognise it next time.** Anyone asking for "readable tab names" for instance or skill pages: the answer is
the URI, not the webview. Do not rename the URI path without a translation layer and a test that round-trips
every page id.
