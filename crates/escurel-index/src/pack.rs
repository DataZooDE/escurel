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

    /// Every indexed base page of `pack_id` (under its reserved
    /// `markdown/base/<pack>/` prefix). The rebase path diffs this
    /// against the incoming version to find orphans.
    pub async fn base_page_ids(&self, pack_id: &str) -> Result<Vec<String>, IndexerError> {
        let prefix = format!("{RESERVED_BASE_PREFIX}{pack_id}/%");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT page_id FROM pages WHERE page_id LIKE ?")?;
        let rows = stmt.query_map(duckdb::params![prefix], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The canonical lane content of `page_id`, or `None` when absent.
    pub async fn page_content(&self, page_id: &str) -> Result<Option<String>, IndexerError> {
        let key = Key::new(self.tenant(), page_id.to_owned())?;
        match self.lane_store().read(&key).await {
            Ok(body) => Ok(Some(
                std::str::from_utf8(&body)
                    .map_err(|_| IndexerError::NotUtf8 {
                        page_id: page_id.to_owned(),
                    })?
                    .to_owned(),
            )),
            Err(_) => Ok(None),
        }
    }

    /// Remove a page entirely: its `pages`/`blocks`/`links` rows and its
    /// canonical lane file. Used by the rebase path for base pages the
    /// new pack version no longer ships — never exposed on the agent
    /// write surface.
    pub async fn remove_page(&self, page_id: &str) -> Result<(), IndexerError> {
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "DELETE FROM blocks WHERE page_id = ?",
                duckdb::params![page_id],
            )?;
            conn.execute(
                "DELETE FROM links WHERE src_page = ?",
                duckdb::params![page_id],
            )?;
            conn.execute(
                "DELETE FROM pages WHERE page_id = ?",
                duckdb::params![page_id],
            )?;
        }
        let key = Key::new(self.tenant(), page_id.to_owned())?;
        self.lane_store().delete(&key).await?;
        Ok(())
    }

    /// The base-layer skill page a tenant overlay page shadows: the pack
    /// page under `markdown/base/` declaring the same slug, or `None`.
    /// Returns `(base_page_id, base_layer_pin, base_frontmatter_json)` —
    /// the drift-visibility payload `expand`/`list_skills` surface
    /// (REQ-LAYER-03: the overlay wins for display, the base value stays
    /// visible, never silently masked).
    pub async fn shadowed_base(
        &self,
        slug: &str,
        own_page_id: &str,
    ) -> Result<Option<(String, String, serde_json::Value)>, IndexerError> {
        let conn = self.conn.lock().await;
        let row: Option<(String, String)> = match conn.query_row(
            "SELECT page_id, frontmatter::VARCHAR FROM pages \
             WHERE page_type = 'skill' AND slug = ? \
               AND page_id LIKE 'markdown/base/%' AND page_id != ? \
             LIMIT 1",
            duckdb::params![slug, own_page_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => Some(v),
            Err(duckdb::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        let Some((page_id, fm_json)) = row else {
            return Ok(None);
        };
        let fm: serde_json::Value = serde_json::from_str(&fm_json)?;
        let pin = fm
            .get("layer")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(Some((page_id, pin, fm)))
    }

    /// The page id of an EXISTING **base-layer** skill page declaring
    /// `skill_id` under a different page id than `landing_page_id`, or
    /// `None`. Import/rebase use this to refuse a pack whose skill
    /// another PACK already provides (base-vs-base has no precedence);
    /// tenant overlay pages are deliberately excluded — a colliding
    /// overlay is the shadow feature, and a LIMIT-1 lookup that could
    /// return the overlay would hide the base-vs-base collision behind
    /// it (codex review P1).
    pub async fn skill_page_conflict(
        &self,
        skill_id: &str,
        landing_page_id: &str,
    ) -> Result<Option<String>, IndexerError> {
        let conn = self.conn.lock().await;
        match conn.query_row(
            "SELECT page_id FROM pages \
             WHERE page_type = 'skill' AND slug = ? AND page_id != ? \
               AND page_id LIKE 'markdown/base/%' \
             LIMIT 1",
            duckdb::params![skill_id, landing_page_id],
            |r| r.get::<_, String>(0),
        ) {
            Ok(existing) => Ok(Some(existing)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Collect the pages of a promotion candidate (REQ-PROMO-01/02),
    /// **default-deny**: every requested id must resolve to a SKILL page
    /// (raw instance data never promotes), carry the curator-set
    /// `promotable: true` marker, and be tenant-authored (`overlay` —
    /// a base-layer page is the hub's, not the spoke's to promote).
    /// One ineligible id refuses the whole request — no silent partial
    /// harvest. Content-hygiene scrubbing happens at the caller (the
    /// same [`pack_scrub_rejection`] the export path runs).
    pub async fn collect_promotion_pages(
        &self,
        skills: &[String],
    ) -> Result<Vec<(String, String)>, IndexerError> {
        // Resolve ids → page ids via the index (lookup only). Every
        // eligibility decision below is made on the CANONICAL LANE
        // CONTENT that gets packed — never on the index row — so there
        // is no gap between what was checked and what leaves the node
        // (agy review: a write between an index check and the lane read
        // could swap eligible content for confidential content).
        let mut page_ids: Vec<String> = Vec::new();
        {
            let conn = self.conn.lock().await;
            for skill in skills {
                let page_id: Option<String> = match conn.query_row(
                    "SELECT page_id FROM pages \
                     WHERE (slug = ? OR page_id = ?) \
                     ORDER BY CASE WHEN page_type = 'skill' THEN 0 ELSE 1 END, \
                              (page_id LIKE 'markdown/base/%') \
                     LIMIT 1",
                    duckdb::params![skill, skill],
                    |r| r.get(0),
                ) {
                    Ok(v) => Some(v),
                    Err(duckdb::Error::QueryReturnedNoRows) => None,
                    Err(e) => return Err(e.into()),
                };
                let Some(page_id) = page_id else {
                    return Err(IndexerError::PromotionNotEligible {
                        reason: format!("`{skill}` names no indexed page"),
                    });
                };
                page_ids.push(page_id);
            }
        }
        page_ids.sort();
        page_ids.dedup();

        let store = self.lane_store();
        let mut out = Vec::with_capacity(page_ids.len());
        for page_id in page_ids {
            let key = Key::new(self.tenant(), page_id.clone())?;
            let body = store.read(&key).await?;
            let content = std::str::from_utf8(&body)
                .map_err(|_| IndexerError::NotUtf8 {
                    page_id: page_id.clone(),
                })?
                .to_owned();
            // Verify WHAT YOU PACK: parse the exact bytes that will be
            // bundled and gate on their frontmatter.
            let parsed =
                escurel_md::parse(&content).map_err(|_| IndexerError::PromotionNotEligible {
                    reason: format!("`{page_id}` does not parse as escurel markdown"),
                })?;
            if parsed.frontmatter.page_type != escurel_md::PageType::Skill {
                return Err(IndexerError::PromotionNotEligible {
                    reason: format!(
                        "`{page_id}` is not a skill page — raw instance data never \
                         promotes (default policy); only firm-curated skills and \
                         structural patterns leave the node"
                    ),
                });
            }
            if parsed
                .frontmatter
                .fields
                .get("promotable")
                .and_then(escurel_md::YamlValue::as_bool)
                != Some(true)
            {
                return Err(IndexerError::PromotionNotEligible {
                    reason: format!(
                        "skill `{page_id}` does not carry `promotable: true`; the \
                         marker is set by a curator, never by default"
                    ),
                });
            }
            if parsed
                .frontmatter
                .fields
                .get("layer")
                .and_then(escurel_md::YamlValue::as_str)
                .is_some_and(|l| l.starts_with("base@"))
            {
                return Err(IndexerError::PromotionNotEligible {
                    reason: format!(
                        "skill `{page_id}` is base-layer pack content — it is the \
                         hub's, not this node's to promote"
                    ),
                });
            }
            let rel = page_id
                .strip_prefix("markdown/")
                .unwrap_or(page_id.as_str())
                .to_owned();
            out.push((rel, content));
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
