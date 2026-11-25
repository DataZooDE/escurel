-- Drafts: the person who asked for the run an agent author worked in (`requested_by`), resolved by the gateway
-- when the draft is filed from the run's root event (gateway-stamped `run_requester`, `asked_by` or `captured_by`).
-- Separation of duties treats them as an author. NULL outside a run or for a run no person asked for. Applied
-- ONLY by `Migrator::ensure_draft_requested_by` (presence-checked, CHECKPOINTed, like 0024).
ALTER TABLE drafts ADD COLUMN IF NOT EXISTS requested_by VARCHAR;
