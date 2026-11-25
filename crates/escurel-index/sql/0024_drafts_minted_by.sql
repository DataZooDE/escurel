-- Drafts: who minted the agent token that filed it (`minted_by`): the token's `act.sub` when the drafting subject is
-- an `agent:<id>` token, i.e. whoever called the mint tool (a person, or the runner for its per-run tokens). Stamped by the gateway from the
-- verified token, never from an argument. Separation of duties treats the minter as an author: the person who
-- minted the token does not accept what it proposed. NULL for a draft no machine filed.
--
-- Applied ONLY by `Migrator::ensure_draft_minted_by` — presence-checked and CHECKPOINTed, because
-- `drafts.created_at` carries a function-valued DEFAULT and an ALTER left in the WAL is unreplayable (see 0017).
ALTER TABLE drafts ADD COLUMN IF NOT EXISTS minted_by VARCHAR;
