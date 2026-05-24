//! Pure-relational read tools on the indexed `pages` table.
//!
//! M2.4 ships the two simplest of the agent contract's seven read
//! tools (`docs/contract/agent-interface.md §The tool surface`):
//!
//! - [`Indexer::list_skills`] — the Tier-1 skill catalogue.
//! - [`Indexer::list_instances`] — list a skill's instances, with
//!   optional `at`-based ordering and a row limit.
//!
//! Neither needs the embedder. Full filter expressions (`>=`, `in`,
//! `null`) and ordering by arbitrary frontmatter fields land in a
//! later M2 PR, once the `frontmatter_index` table is exercised.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// One skill page, projected for the Tier-1 catalogue.
///
/// `is_event_typed` mirrors the convenience flag the spec defines in
/// `docs/spec/protocol.md §list_skills`: true when `at` is in
/// `required_frontmatter`. Agents use it to pick the
/// event-log code path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInfo {
    pub id: String,
    pub description: String,
    pub required_frontmatter: Vec<String>,
    pub optional_frontmatter: Vec<String>,
    pub is_event_typed: bool,
}

/// One instance page, projected for [`Indexer::list_instances`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    pub page_id: String,
    pub skill: String,
    /// Full frontmatter as a JSON object (for the agent to project
    /// skill-specific fields).
    pub frontmatter: serde_json::Value,
    /// Mirrored `frontmatter.at` if the instance is event-typed,
    /// else `None`. RFC 3339 string form.
    pub at: Option<String>,
}

/// Sort direction for [`Indexer::list_instances`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDir {
    Asc,
    Desc,
}

impl Indexer {
    /// Return every skill page in the tenant's index.
    ///
    /// The spec lists this under the "kind" axis as the Tier-1
    /// catalogue (`docs/contract/agent-interface.md §The tool
    /// surface`). One row per `page_type = 'skill'`.
    pub async fn list_skills(&self) -> Result<Vec<SkillInfo>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT page_id, frontmatter::VARCHAR \
             FROM pages \
             WHERE page_type = 'skill' \
             ORDER BY page_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (page_id, fm_json) = row?;
            let fm: serde_json::Value = serde_json::from_str(&fm_json)?;
            out.push(SkillInfo {
                id: skill_id(&fm).unwrap_or(page_id),
                description: fm
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                required_frontmatter: string_array_field(&fm, "required_frontmatter"),
                optional_frontmatter: string_array_field(&fm, "optional_frontmatter"),
                is_event_typed: string_array_field(&fm, "required_frontmatter")
                    .iter()
                    .any(|k| k == "at"),
            });
        }
        Ok(out)
    }

    /// Return instances of `skill`, optionally ordered by `at_ts`
    /// and capped at `limit` rows.
    ///
    /// Ordering uses the denormalised `at_ts` column on `pages`
    /// (mirrored from `frontmatter.at` at write time), so the
    /// event-log scan `list_instances('meeting', Some(Desc),
    /// Some(50))` is index-served by the `pages_skill_at`
    /// composite index.
    pub async fn list_instances(
        &self,
        skill: &str,
        order_by_at: Option<OrderDir>,
        limit: Option<usize>,
    ) -> Result<Vec<InstanceInfo>, IndexerError> {
        let order_sql = match order_by_at {
            Some(OrderDir::Asc) => " ORDER BY at_ts ASC NULLS LAST, page_id ASC",
            Some(OrderDir::Desc) => " ORDER BY at_ts DESC NULLS LAST, page_id ASC",
            None => " ORDER BY page_id",
        };
        // `limit` is a usize from our own code, not user input, so
        // splicing it as `format!` is safe; we still cap it at a
        // reasonable max so a caller mistake doesn't OOM us.
        let limit_sql = limit
            .map(|n| format!(" LIMIT {}", n.min(10_000)))
            .unwrap_or_default();
        let sql = format!(
            "SELECT page_id, skill, frontmatter::VARCHAR \
             FROM pages \
             WHERE page_type = 'instance' AND skill = ?{order_sql}{limit_sql}",
        );

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![skill], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (page_id, skill, fm_json) = row?;
            let fm: serde_json::Value = serde_json::from_str(&fm_json)?;
            // Project `at` from the frontmatter JSON — the at_ts
            // column on `pages` is a TIMESTAMP and reformatting it
            // back to RFC 3339 would risk drifting from what the
            // agent originally wrote. The JSON copy is canonical.
            let at = fm
                .get("at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            out.push(InstanceInfo {
                page_id,
                skill,
                frontmatter: fm,
                at,
            });
        }
        Ok(out)
    }
}

fn skill_id(fm: &serde_json::Value) -> Option<String> {
    fm.get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn string_array_field(fm: &serde_json::Value, key: &str) -> Vec<String> {
    fm.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
