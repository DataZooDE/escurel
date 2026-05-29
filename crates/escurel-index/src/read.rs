//! Pure-relational read tools on the indexed `pages` table.
//!
//! Four of the seven agent contract read tools
//! (`docs/contract/agent-interface.md §The tool surface`) ship here:
//!
//! - [`Indexer::list_skills`] — Tier-1 skill catalogue.
//! - [`Indexer::list_instances`] — list a skill's instances, with
//!   optional `at`-based ordering and a row limit.
//! - [`Indexer::resolve`] — parse a `[[skill::id]]` wikilink and
//!   look up its target page.
//! - [`Indexer::expand`] — fetch a page's full body + frontmatter
//!   + outbound wikilinks.
//!
//! None need the embedder. Full filter expressions (`>=`, `in`,
//! `null`), ordering by arbitrary frontmatter fields, and
//! `neighbours` land in later M2 PRs.

use duckdb::params;
use escurel_md::PageType;
use escurel_md::wikilink::{WikilinkParsed, parse_wikilinks};

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

    /// Return instances of `skill`, optionally ordered by `at_ts`,
    /// filtered by a single frontmatter `(key, value)`, and capped
    /// at `limit` rows.
    ///
    /// Ordering uses the denormalised `at_ts` column on `pages`
    /// (mirrored from `frontmatter.at` at write time), so the
    /// event-log scan `list_instances('meeting', Some(Desc), …)`
    /// is index-served by the `pages_skill_at` composite index.
    ///
    /// `filter` matches a top-level frontmatter field by its string
    /// value, e.g. `Some(("source", "gmail"))` for the source-inbox
    /// view. It compares `json_extract_string(frontmatter, '$.<key>')`
    /// directly against the canonical stored frontmatter — there is no
    /// separate `frontmatter_index` to keep in sync.
    ///
    /// `as_of` is the time-travel cut: when `Some`, only instances born
    /// at or before that RFC 3339 instant are returned
    /// (`at_ts <= as_of`). Untimed instances (`at_ts IS NULL`) are
    /// always present — they are not events on the timeline.
    pub async fn list_instances(
        &self,
        skill: &str,
        order_by_at: Option<OrderDir>,
        limit: Option<usize>,
        filter: Option<(&str, &str)>,
        as_of: Option<&str>,
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
        let filter_sql = if filter.is_some() {
            " AND json_extract_string(frontmatter, ?) = ?"
        } else {
            ""
        };
        let as_of_sql = if as_of.is_some() {
            " AND (at_ts <= ? OR at_ts IS NULL)"
        } else {
            ""
        };
        let sql = format!(
            "SELECT page_id, skill, frontmatter::VARCHAR \
             FROM pages \
             WHERE page_type = 'instance' AND skill = ?{filter_sql}{as_of_sql}{order_sql}{limit_sql}",
        );

        // Bind order matches the `?` order in `sql`: skill, then the
        // filter path + value, then the as_of cut.
        let mut binds: Vec<String> = vec![skill.to_owned()];
        if let Some((key, value)) = filter {
            binds.push(format!("$.{key}"));
            binds.push(value.to_owned());
        }
        if let Some(ts) = as_of {
            binds.push(ts.to_owned());
        }

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), |row| {
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

/// Reference to a single page in the index. The shape that
/// `resolve` / `expand` / `neighbours` return when a page is
/// known to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRef {
    pub page_id: String,
    /// Mutable human-friendly id from `frontmatter.id`. `None` for
    /// pages whose frontmatter doesn't declare one.
    pub slug: Option<String>,
    pub skill: String,
    pub page_type: PageType,
}

/// Result of [`Indexer::resolve`].
///
/// `parsed` is the wikilink decomposed via `escurel-md::wikilink`.
/// `page` is the resolved target if it exists in the index; `None`
/// when no page matches the `(skill, id)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWikilink {
    pub parsed: WikilinkParsed,
    pub page: Option<PageRef>,
}

impl ResolvedWikilink {
    #[must_use]
    pub fn exists(&self) -> bool {
        self.page.is_some()
    }
}

/// Result of [`Indexer::expand`]: the full page body, its
/// frontmatter, the page's blocks, and the parsed outbound
/// wikilinks from the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedPage {
    pub page: PageRef,
    pub frontmatter: serde_json::Value,
    pub body: String,
    pub blocks: Vec<BlockInfo>,
    pub wikilinks_out: Vec<WikilinkParsed>,
}

/// One block inside a page (anchor + content).
///
/// Today's indexer writes a single block per page (anchor =
/// `blk-0`, content = full body). Block-anchor splitting lands
/// in a later PR; the API shape already supports the multi-block
/// future.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInfo {
    pub anchor: String,
    pub content: String,
}

impl Indexer {
    /// Parse a `[[skill::id]]` wikilink and look up its target.
    ///
    /// Returns the parsed wikilink alongside the resolved [`PageRef`]
    /// if the page exists. A bare `[[id]]` resolves against `slug`
    /// only (no skill constraint), so it succeeds for any page in
    /// any skill whose `frontmatter.id` matches.
    pub async fn resolve(&self, wikilink: &str) -> Result<ResolvedWikilink, IndexerError> {
        let mut parsed = parse_wikilinks(wikilink);
        let parsed = parsed
            .pop()
            .ok_or_else(|| IndexerError::Md(escurel_md::ParseError::MissingFrontmatter))?;
        // (We reuse ParseError::MissingFrontmatter as the closest
        // "input didn't parse" variant; a dedicated WikilinkParseError
        // can land if/when other callers need to distinguish.)

        let id = match parsed.id.as_deref() {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => {
                // No id segment — resolution is meaningless.
                return Ok(ResolvedWikilink { parsed, page: None });
            }
        };

        let conn = self.conn.lock().await;
        let row = match parsed.skill.as_deref() {
            Some(skill) => conn
                .query_row(
                    "SELECT page_id, slug, skill, page_type \
                     FROM pages WHERE skill = ? AND slug = ? LIMIT 1",
                    params![skill, id],
                    page_ref_from_row,
                )
                .ok(),
            None => conn
                .query_row(
                    "SELECT page_id, slug, skill, page_type \
                     FROM pages WHERE slug = ? LIMIT 1",
                    params![id],
                    page_ref_from_row,
                )
                .ok(),
        };

        Ok(ResolvedWikilink { parsed, page: row })
    }

    /// Fetch the full body of `page_id` plus its frontmatter and
    /// outbound wikilinks. Returns `Ok(None)` when no page with that
    /// `page_id` is in the index.
    ///
    /// The body comes from the `blocks` table (which mirrors the
    /// parsed markdown body), not from a fresh LaneStore read — this
    /// keeps `expand` index-served and avoids a second I/O round-trip.
    pub async fn expand(
        &self,
        page_id: &str,
        as_of: Option<&str>,
    ) -> Result<Option<ExpandedPage>, IndexerError> {
        let conn = self.conn.lock().await;

        // Page row. With an `as_of` cut, a page whose `at_ts` is after
        // the cut is "not born yet" and resolves to None; untimed pages
        // (skills, non-event instances) stay visible.
        let as_of_sql = if as_of.is_some() {
            " AND (at_ts <= ? OR at_ts IS NULL)"
        } else {
            ""
        };
        let page_sql = format!(
            "SELECT page_id, slug, skill, page_type, frontmatter::VARCHAR \
             FROM pages WHERE page_id = ?{as_of_sql}"
        );
        let mut page_binds: Vec<String> = vec![page_id.to_owned()];
        if let Some(ts) = as_of {
            page_binds.push(ts.to_owned());
        }
        let page_with_fm = conn
            .query_row(
                &page_sql,
                duckdb::params_from_iter(page_binds.iter()),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .ok();
        let Some((page_id, slug, skill, page_type_str, fm_json)) = page_with_fm else {
            return Ok(None);
        };
        let page_type = match page_type_str.as_str() {
            "skill" => PageType::Skill,
            _ => PageType::Instance,
        };
        let frontmatter: serde_json::Value = serde_json::from_str(&fm_json)?;

        // Blocks for the page (single-block-per-page today, but the
        // shape supports multi-block).
        let mut stmt =
            conn.prepare("SELECT anchor, body FROM blocks WHERE page_id = ? ORDER BY ordinal")?;
        let block_rows = stmt.query_map(params![&page_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, String>(1)?,
            ))
        })?;
        let mut blocks = Vec::new();
        let mut body = String::new();
        for r in block_rows {
            let (anchor, content) = r?;
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&content);
            blocks.push(BlockInfo { anchor, content });
        }

        let wikilinks_out = parse_wikilinks(&body);

        Ok(Some(ExpandedPage {
            page: PageRef {
                page_id,
                slug,
                skill,
                page_type,
            },
            frontmatter,
            body,
            blocks,
            wikilinks_out,
        }))
    }
}

/// Direction filter for [`Indexer::neighbours`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Only inbound edges (links whose `dst` is the queried page).
    In,
    /// Only outbound edges (links whose `src` is the queried page).
    Out,
    /// Both directions; output is the union, no de-duplication.
    Both,
}

/// One link in the typed graph. The shape matches
/// `docs/spec/protocol.md §neighbours` minus the `target_frontmatter_excerpt`
/// (added in a later PR once the read path needs it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// `pages.page_id` of the link's source.
    pub src_page: String,
    /// The wikilink's `id` segment — i.e. the target's `slug`. To
    /// hop to the resolved target page, call
    /// [`Indexer::resolve`] with `[[link_skill::dst_page]]`.
    pub dst_page: String,
    /// `''` for bare wikilinks, `<skill>` for typed.
    pub link_skill: String,
    /// `@version` segment of the wikilink, when present.
    pub link_version: Option<String>,
    /// `#anchor` segment of the wikilink, when present. Empty
    /// string in storage maps back to `None` here.
    pub dst_anchor: Option<String>,
}

impl Indexer {
    /// Return links touching `page_id` in the chosen `direction`,
    /// optionally filtered to a single `link_skill`.
    ///
    /// `direction = Out` queries `WHERE src_page = page_id` —
    /// fast because that is the most selective index.
    /// `direction = In` first resolves `page_id` to its
    /// `(skill, slug)` pair on the `pages` table (since the
    /// `links` table records `dst_page` as the wikilink id /
    /// slug, not the canonical `page_id`), then queries
    /// `WHERE dst_page = slug AND link_skill = skill`.
    /// `direction = Both` is the union of the two.
    ///
    /// Bare-link disambiguation (a `[[acme-corp]]` link could
    /// point at any `acme-corp`-slugged page) is out of scope here;
    /// inbound queries today only see links whose `link_skill`
    /// equals the resolved page's skill.
    ///
    /// `as_of` time-travels the graph: an edge is only visible when its
    /// **source** page was born at or before the cut
    /// (`src.at_ts <= as_of`, or the source is untimed). Inbound edges
    /// from pages not yet born are thus hidden, so the link graph
    /// reflects what existed at that instant.
    pub async fn neighbours(
        &self,
        page_id: &str,
        direction: Direction,
        link_skill_filter: Option<&str>,
        as_of: Option<&str>,
    ) -> Result<Vec<Edge>, IndexerError> {
        let conn = self.conn.lock().await;

        let target = if matches!(direction, Direction::In | Direction::Both) {
            conn.query_row(
                "SELECT slug, skill FROM pages WHERE page_id = ?",
                params![page_id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            )
            .ok()
        } else {
            None
        };

        let mut edges = Vec::new();
        let want_out = matches!(direction, Direction::Out | Direction::Both);
        let want_in = matches!(direction, Direction::In | Direction::Both);

        // The as_of cut keeps only links whose source page existed at
        // the instant — `src.at_ts <= as_of` or untimed. Expressed as an
        // EXISTS so the SELECT column order (read by `edge_from_row`)
        // never shifts.
        let as_of_exists = " AND EXISTS (SELECT 1 FROM pages p \
             WHERE p.page_id = l.src_page AND (p.at_ts <= ? OR p.at_ts IS NULL))";

        if want_out {
            let mut sql = String::from(
                "SELECT l.src_page, l.dst_page, l.dst_anchor, l.link_skill, l.link_version \
                 FROM links l WHERE l.src_page = ?",
            );
            let mut binds: Vec<String> = vec![page_id.to_owned()];
            if let Some(ls) = link_skill_filter {
                sql.push_str(" AND l.link_skill = ?");
                binds.push(ls.to_owned());
            }
            if let Some(ts) = as_of {
                sql.push_str(as_of_exists);
                binds.push(ts.to_owned());
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), edge_from_row)?;
            for r in rows {
                edges.push(r?);
            }
        }

        if want_in && let Some((Some(slug), skill)) = target {
            let mut sql = String::from(
                "SELECT l.src_page, l.dst_page, l.dst_anchor, l.link_skill, l.link_version \
                 FROM links l WHERE l.dst_page = ? AND l.link_skill = ?",
            );
            let mut binds: Vec<String> = vec![slug, skill];
            if let Some(ls) = link_skill_filter {
                // Filter further if a more specific link_skill was
                // requested. `link_skill` here is the dst's own
                // skill; the filter only narrows it further.
                sql.push_str(" AND l.link_skill = ?");
                binds.push(ls.to_owned());
            }
            if let Some(ts) = as_of {
                sql.push_str(as_of_exists);
                binds.push(ts.to_owned());
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), edge_from_row)?;
            for r in rows {
                edges.push(r?);
            }
        }

        Ok(edges)
    }
}

fn edge_from_row(row: &duckdb::Row<'_>) -> duckdb::Result<Edge> {
    let dst_anchor: String = row.get(2)?;
    let dst_anchor = if dst_anchor.is_empty() {
        None
    } else {
        Some(dst_anchor)
    };
    Ok(Edge {
        src_page: row.get(0)?,
        dst_page: row.get(1)?,
        dst_anchor,
        link_skill: row.get(3)?,
        link_version: row.get(4)?,
    })
}

fn page_ref_from_row(row: &duckdb::Row<'_>) -> duckdb::Result<PageRef> {
    let page_id: String = row.get(0)?;
    let slug: Option<String> = row.get(1)?;
    let skill: String = row.get(2)?;
    let page_type_str: String = row.get(3)?;
    let page_type = match page_type_str.as_str() {
        "skill" => PageType::Skill,
        _ => PageType::Instance,
    };
    Ok(PageRef {
        page_id,
        slug,
        skill,
        page_type,
    })
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
