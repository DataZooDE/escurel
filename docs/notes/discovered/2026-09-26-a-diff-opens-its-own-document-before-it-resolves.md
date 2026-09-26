# A diff opens its own document before `vscode.diff` resolves

**Symptom.** Opening one draft review in the VS Code extension rendered every
comment thread twice: two review comments produced four thread widgets in the
proposed pane, on a single clean open. Nothing in the unit tests caught it —
the model that groups comments into threads was correct, and returned two.

**Cause.** Two call sites load a draft's comments, and they overlap:

- `ReviewController.openDraftReview` awaits `vscode.commands.executeCommand('vscode.diff', …)`
  and then loads the comments, and
- a `vscode.workspace.onDidOpenTextDocument` handler loads them too, so a diff
  that VS Code restores on window reload still gets its threads.

`vscode.diff` opens the proposed document *while it is still running*, so the
document-open handler fires inside the first call's `await` — not after it. The
loader disposed the draft's existing threads and then created new ones with an
`await` between the two halves, so both calls saw "no existing threads" and each
created a full set.

**Fix.** Two parts, both needed:

1. Keep the in-flight load per draft in a map; a second caller awaits the first
   rather than starting its own.
2. Dispose the old threads and create the new ones in the same synchronous turn,
   after the fetch, so the pair can never interleave.

The explicit open also marks the draft as "being opened" so the document-open
handler skips it; a window reload does not go through `openDraftReview`, so that
handler stays the sole loader for restored tabs.

**How to recognise it next time.** Any `onDidOpenTextDocument` /
`onDidChangeVisibleEditors` handler that reacts to a document the extension
itself is in the middle of opening will run *during* the opening call, not after.
Treat a dispose-then-create pair that straddles an `await` as a race by default.

**Related.** A polling helper that rejects an empty array as "not settled yet"
cannot observe a queue being *emptied* — the M2 integration suite's
already-cleared Awaiting check timed out for exactly that reason. Wrap the value
when the awaited state is emptiness.
