-- Branches: who an agent author acted for (`minted_by`, the token's `act.sub` when the opening subject is an
-- `agent:<id>` token), as on drafts (0024). Separation of duties treats the minter as an author of the branch.
-- NULL for a branch a person opened. Applied ONLY by `Migrator::ensure_branches` (presence-checked, CHECKPOINTed:
-- `branches.created_at` carries a function-valued DEFAULT).
ALTER TABLE branches ADD COLUMN IF NOT EXISTS minted_by VARCHAR;
