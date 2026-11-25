-- Branches: the person who asked for the run an agent author worked in (`requested_by`), as on drafts (0027).
-- Separation of duties treats them as an author of the branch. NULL outside a run. Applied ONLY by
-- `Migrator::ensure_branches` (presence-checked, CHECKPOINTed, like 0025).
ALTER TABLE branches ADD COLUMN IF NOT EXISTS requested_by VARCHAR;
