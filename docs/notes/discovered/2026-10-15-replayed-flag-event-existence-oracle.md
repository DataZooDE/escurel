# `capture_event` `replayed:true` told a caller who may not read an event that its id was taken

## Symptom
Under `ESCUREL_EVENT_ACL=enforce`, `capture_event` with an `event_id` that already exists answers the caller's OWN
submission (so a guesser learns nothing from the body), but the answer still carried `replayed: true`. A caller could
probe event ids and learn which exist, for events they may not read.

## Cause
`replayed` is computed from the stored row (`get_event`) before the event-ACL is applied, and was added to the answer
whether the caller was shown the stored row or the echo.

## Fix
`replayed` is only attached when the caller is shown the stored row (visible, or ACL mode `log`). A hidden collision now
reads exactly like a first capture. The owner's retry still gets `replayed: true`. Test:
`event_acl::a_hidden_collision_echo_does_not_say_replayed` (red first: bob's echo said `replayed: true`).

## Recognise it next time
Any flag derived from the stored row that is copied into an answer the ACL has already replaced with an echo.

## Changelog lines for the release notes
- Security: under `ESCUREL_EVENT_ACL=enforce`, `capture_event` no longer sets `replayed: true` on the answer to a caller
  who may not read the stored event, so a guessed `event_id` no longer reveals that it exists.
