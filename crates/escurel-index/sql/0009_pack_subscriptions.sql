-- Pack subscriptions (REQ-SUB-01): one row per subscribed skill pack,
-- pinning the imported version. Like `external_credentials` this is a
-- SEPARATE canonical input (not derivable from `pages/`): `rebuild`
-- must NOT drop it — it is the provenance record that says which
-- `markdown/base/<pack>/…` pages are pack-managed and at which pin
-- (INV-DERIV: a tenant rebuilds from overlay pages + subscribed packs).
CREATE TABLE IF NOT EXISTS pack_subscriptions (
    pack_id       VARCHAR PRIMARY KEY,
    version       INTEGER NOT NULL,
    vertical      VARCHAR NOT NULL,
    publisher     VARCHAR NOT NULL,
    content_hash  VARCHAR NOT NULL,
    signature     VARCHAR NOT NULL,
    subscribed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
