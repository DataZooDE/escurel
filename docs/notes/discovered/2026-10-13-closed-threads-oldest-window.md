# The Closed group read the OLDEST 200 close marks and never loaded at start

**Symptom.** Found by the second crew review. The Threads outline folded the `escurel:thread-closed` listing
with `list_events{limit: 200}`, whose default order is oldest-first, and refreshed it only when a close landed
live, after this window's own close/reopen, or when the config/auth changed. So a fresh window showed an empty
Closed group until something happened, and a tenant with more than 200 marks lost every later mark: a reopen
beyond mark 200 left the thread "closed" forever, and recent closes never appeared.

**Fix.** `loadClosedThreads` (src/views/closedThreads.ts) reads newest-first, follows the cursor for at most
`maxPages` (5) pages of 200, and reports `truncated` when older marks were left unread. The fold is
order-independent (latest mark per root wins), so the newest pages are the ones that matter. It runs at
activation, on every (re)connect of the live socket (`LiveCoordinator.onDidConnect`), live on a close, and
after the window's own close/reopen. A newer refresh wins over one in flight (sequence guard); a failed refresh
keeps the previous list of the same tenant; a gateway/auth change clears the list first, so another tenant's
closed threads are never kept.

**Recognise it next time.** A group built from a label listing must say which END of the history it reads and
must load on connect, not only on events: test it with more marks than one page and with the reopen last.

## Changelog lines for the release notes
- VS Code: the Closed threads group loads when the window starts and after every reconnect, and reads the newest
  marks first (a reopen or a recent close in a long history is no longer missed).
