//! Lease fence for writes made by a worker that may have been replaced.
//!
//! A skill declares which fields make an instance a lease:
//!
//! ```yaml
//! lease:
//!   holder: attempt          # field naming the current holder
//!   expires: lease_until     # field holding the expiry, Unix seconds
//!   while: {status: running} # optional: the lease is live only in this state
//! ```
//!
//! [`Indexer::lease_is_current`] answers whether a given holder still owns
//! such a page right now. A page of a skill that declares no lease, a missing
//! page, another holder, an expired lease and an inactive state all answer no.
//! The caller takes the page-write gate first so the answer cannot go stale
//! between the check and the write it protects.

use escurel_md::{PageKind, YamlValue, parse};

use crate::{Indexer, IndexerError};

/// The refusal code for a write whose lease no longer holds.
pub const LEASE_FENCE: &str = "lease_fence";

/// A parsed `lease:` declaration.
struct LeaseDecl {
    holder: String,
    expires: String,
    active: Option<(String, String)>,
}

fn parse_decl(value: &serde_json::Value) -> Option<LeaseDecl> {
    let holder = value.get("holder")?.as_str()?.to_owned();
    let expires = value.get("expires")?.as_str()?.to_owned();
    let active = value
        .get("while")
        .and_then(serde_json::Value::as_object)
        .and_then(|m| {
            let (k, v) = m.iter().next()?;
            Some((k.clone(), v.as_str()?.to_owned()))
        });
    Some(LeaseDecl {
        holder,
        expires,
        active,
    })
}

/// Lint a skill's `lease:` declaration; `None` when it is well formed.
pub(crate) fn lease_declaration_problem(value: &serde_json::Value) -> Option<&'static str> {
    let ok = parse_decl(value).is_some()
        && value
            .get("while")
            .is_none_or(|w| w.as_object().is_some_and(|m| m.len() == 1));
    (!ok).then_some("`lease:` needs string `holder:` and `expires:` fields and an optional single-entry `while:` map")
}

impl Indexer {
    /// Whether `holder` currently owns the lease page `page_id`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] for an underlying store or index failure.
    pub async fn lease_is_current(
        &self,
        page_id: &str,
        holder: &str,
        now_unix: u64,
    ) -> Result<bool, IndexerError> {
        let Some(content) = self.read_page_markdown(page_id).await? else {
            return Ok(false);
        };
        let Ok(parsed) = parse(&content) else {
            return Ok(false);
        };
        if parsed.frontmatter.page_kind != PageKind::Instance {
            return Ok(false);
        }
        let fields = &parsed.frontmatter.fields;
        let Some(skill) = fields.get("skill").and_then(YamlValue::as_str) else {
            return Ok(false);
        };
        let decl = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND slug = ? AND NOT COALESCE(deleted, false) \
                 ORDER BY (page_id LIKE 'markdown/base/%') ASC LIMIT 1",
            )?;
            let mut rows = stmt.query([skill])?;
            match rows.next()? {
                Some(row) => match row.get::<_, Option<String>>(0)? {
                    Some(text) => {
                        let fm: serde_json::Value = serde_json::from_str(&text)?;
                        fm.get("lease").and_then(parse_decl)
                    }
                    None => None,
                },
                None => None,
            }
        };
        let Some(decl) = decl else {
            return Ok(false);
        };
        let text = |name: &str| fields.get(name).and_then(YamlValue::as_str);
        Ok(text(&decl.holder) == Some(holder)
            && fields
                .get(&decl.expires)
                .and_then(YamlValue::as_u64)
                .is_some_and(|until| until > now_unix)
            && decl
                .active
                .as_ref()
                .is_none_or(|(k, v)| text(k) == Some(v.as_str())))
    }
}
