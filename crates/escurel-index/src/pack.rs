//! Pack-side page selection + content hygiene (REQ-PACK-01/04).
//!
//! A **skill pack** bundles a skill subtree — the skill pages named by
//! the exporter plus (optionally) their instances — as the Company
//! Model's unit of distribution. This module owns the two halves that
//! live close to the data:
//!
//! * [`Indexer::collect_pack_pages`] — deterministic selection of the
//!   subtree, contents read from the canonical LaneStore markdown.
//! * [`pack_scrub_rejection`] — the fail-closed content-hygiene check
//!   (INV-SECRETFREE): credential-shaped strings abort the export.
//!   Deliberately deterministic — no LLM, no heuristic scoring — and
//!   shared by the export path today and the promotion gate (WI-4)
//!   next, so there is exactly one place that decides "this must not
//!   leave the node".
//!
//! Bundling and signing live server-side (`escurel-server/src/pack.rs`);
//! this module never sees the tarball or the secret.

use std::sync::LazyLock;

use escurel_storage::Key;
use regex::Regex;

use crate::indexer::{Indexer, IndexerError};

/// The reserved page-id prefix pack import lands pages under. The agent
/// write surface (`update_page`, `open_session`) refuses this prefix
/// **statically** — even for page ids no import has created yet — so a
/// racing import can neither be squatted nor bypassed (the TOCTOU
/// finding from the layer-model review). Only the import path writes
/// here.
pub const RESERVED_BASE_PREFIX: &str = "markdown/base/";

/// One subscribed pack: the pin recorded in the `pack_subscriptions`
/// canonical table (REQ-SUB-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSubscription {
    pub pack_id: String,
    pub version: u32,
    pub vertical: String,
    pub publisher: String,
    pub content_hash: String,
    pub signature: String,
}

/// A DSN carrying inline credentials (`scheme://user:pass@host/…`,
/// including the empty-password `user:@host` shape — agy review). The
/// one shape REQ-SQL-05 already banned from markdown; packs ban it
/// again at the boundary, fail-closed.
static DSN_WITH_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:[^/\s@]*@").expect("static regex compiles")
});

/// A PEM/PGP private-key block header, case-insensitive, with or
/// without the PGP `… BLOCK` suffix (agy review: the strict
/// `KEY-----` anchor missed `-----BEGIN PGP PRIVATE KEY BLOCK-----`).
static PRIVATE_KEY_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----")
        .expect("static regex compiles")
});

/// A key-value connection-string credential (`Password=hunter2;`,
/// `pwd=…`). Restricted to `=`-style assignments so ordinary prose and
/// YAML documentation (`token: set this via the registry`) doesn't
/// false-positive; the promotion gate extends the deny set further.
static KV_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(password|passwd|pwd)\s*=\s*[^\s;"']+"#).expect("static regex compiles")
});

/// Fail-closed content-hygiene check for anything leaving the node in a
/// pack: `Some(reason)` when `content` must NOT be exported. The deny
/// set is deliberately small and deterministic (a false positive is a
/// refused export an operator can fix; a false negative is a leaked
/// credential) and grows with the promotion gate.
#[must_use]
pub fn pack_scrub_rejection(path: &str, content: &str) -> Option<String> {
    if DSN_WITH_CREDENTIALS.is_match(content) {
        return Some(format!(
            "pack_secret_detected: page `{path}` contains a DSN with inline \
             credentials; packs are secret-free (INV-SECRETFREE) — register \
             the source via the credential registry and reference it by name"
        ));
    }
    if PRIVATE_KEY_BLOCK.is_match(content) {
        return Some(format!(
            "pack_secret_detected: page `{path}` contains a PEM/PGP private-key \
             block; packs are secret-free (INV-SECRETFREE)"
        ));
    }
    if KV_PASSWORD.is_match(content) {
        return Some(format!(
            "pack_secret_detected: page `{path}` contains a `password=`-style \
             connection-string credential; packs are secret-free (INV-SECRETFREE)"
        ));
    }
    None
}

impl Indexer {
    /// Record (or refresh) a pack subscription pin. `REPLACE` semantics:
    /// re-importing the same pack upserts its row (idempotent import).
    pub async fn record_pack_subscription(
        &self,
        sub: &PackSubscription,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO pack_subscriptions \
             (pack_id, version, vertical, publisher, content_hash, signature) \
             VALUES (?, ?, ?, ?, ?, ?)",
            duckdb::params![
                sub.pack_id,
                sub.version,
                sub.vertical,
                sub.publisher,
                sub.content_hash,
                sub.signature,
            ],
        )?;
        Ok(())
    }

    /// Every subscribed pack, ordered by pack id.
    pub async fn list_pack_subscriptions(&self) -> Result<Vec<PackSubscription>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT pack_id, version, vertical, publisher, content_hash, signature \
             FROM pack_subscriptions ORDER BY pack_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PackSubscription {
                pack_id: r.get(0)?,
                version: u32::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                vertical: r.get(2)?,
                publisher: r.get(3)?,
                content_hash: r.get(4)?,
                signature: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Collect the pages of a pack subtree, deterministically ordered by
    /// path: for each skill id in `skills`, its skill page plus — when
    /// `include_instances` — every instance page of that skill. Paths
    /// are lane-relative (`skills/<id>.md`, `instances/<skill>/<id>.md`,
    /// i.e. the page id without its `markdown/` prefix) so an importer
    /// chooses its own landing prefix; contents come from the canonical
    /// LaneStore markdown (the source of truth `rebuild`/`audit` read).
    ///
    /// Fails when a named skill has no skill page — a pack that silently
    /// dropped a requested skill would look complete and not be.
    pub async fn collect_pack_pages(
        &self,
        skills: &[String],
        include_instances: bool,
    ) -> Result<Vec<(String, String)>, IndexerError> {
        let mut page_ids: Vec<String> = Vec::new();
        {
            let conn = self.conn.lock().await;
            for skill in skills {
                let skill_page: Option<String> = match conn.query_row(
                    "SELECT page_id FROM pages \
                     WHERE page_type = 'skill' AND (slug = ? OR page_id = ?) \
                     LIMIT 1",
                    duckdb::params![skill, skill],
                    |r| r.get(0),
                ) {
                    Ok(v) => Some(v),
                    Err(duckdb::Error::QueryReturnedNoRows) => None,
                    Err(e) => return Err(e.into()),
                };
                let Some(skill_page) = skill_page else {
                    return Err(IndexerError::PackSkillMissing {
                        skill: skill.clone(),
                    });
                };
                page_ids.push(skill_page);

                if include_instances {
                    let mut stmt = conn.prepare(
                        "SELECT page_id FROM pages \
                         WHERE page_type = 'instance' AND skill = ? \
                         ORDER BY page_id",
                    )?;
                    let rows = stmt.query_map(duckdb::params![skill], |r| r.get::<_, String>(0))?;
                    for row in rows {
                        page_ids.push(row?);
                    }
                }
            }
        }
        page_ids.sort();
        page_ids.dedup();

        let mut out = Vec::with_capacity(page_ids.len());
        let store = self.lane_store();
        for page_id in page_ids {
            let key = Key::new(self.tenant(), page_id.clone())?;
            let body = store.read(&key).await?;
            let content = std::str::from_utf8(&body)
                .map_err(|_| IndexerError::NotUtf8 {
                    page_id: page_id.clone(),
                })?
                .to_owned();
            let rel = page_id
                .strip_prefix("markdown/")
                .unwrap_or(page_id.as_str())
                .to_owned();
            out.push((rel, content));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::pack_scrub_rejection;

    /// The bypass shapes the PR-2 agy review found — pinned so the deny
    /// set can only grow.
    #[test]
    fn scrub_rejects_credential_shapes() {
        for leaky in [
            "postgres://svc:hunter2@db.internal/prod",   // classic DSN
            "postgres://svc:@db.internal/prod",          // empty password
            "-----BEGIN PRIVATE KEY-----",               // PEM
            "-----BEGIN RSA PRIVATE KEY-----",           // PEM, keyed
            "-----BEGIN PGP PRIVATE KEY BLOCK-----",     // PGP suffix
            "-----begin openssh private key-----",       // lowercase
            "Server=db;Password=hunter2;Database=prod;", // key-value
            "pwd = hunter2",                             // spaced key-value
        ] {
            assert!(
                pack_scrub_rejection("skills/x.md", leaky).is_some(),
                "must reject: {leaky}"
            );
        }
    }

    #[test]
    fn scrub_allows_ordinary_documentation() {
        for fine in [
            "Register the source via register_credential and reference it by name.",
            "See https://db.internal/prod for the dashboard.",
            "The `token:` frontmatter key names the owning principal.",
            "password rotation happens quarterly", // prose, no assignment
            "user@example.com sent the report",    // plain email
        ] {
            assert!(
                pack_scrub_rejection("skills/x.md", fine).is_none(),
                "must allow: {fine}"
            );
        }
    }
}
