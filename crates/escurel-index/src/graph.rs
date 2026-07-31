//! Bounded provenance-graph traversals over the `resolved_links` view
//! (ADR-0010).
//!
//! `neighbours` is one-hop; this module adds bounded MULTI-hop reads —
//! "everything this rests on" / "everything derived from this" — used by
//! the `provenance_*` MCP tools. Every traversal is depth-bounded and
//! cycle-guarded (the "no arbitrary traversal" contract): the agent
//! supplies only named scalars + an allow-listed relation filter, never
//! SQL.
//!
//! The queries sit behind a [`GraphBackend`] seam so the identical tool
//! surface can be answered by stock DuckDB **recursive CTEs** (the
//! default, zero-dependency backend) or, once the version spike is
//! green, by DuckPGQ `MATCH`. Only the CTE arm exists today.

use crate::indexer::{Indexer, IndexerError};

/// Hard ceiling on hop depth, whatever `max_hops` a caller asks for. A
/// backstop on traversal cost — the tool default is much smaller.
pub const MAX_HOPS_CEILING: u32 = 12;

/// Which way to walk the provenance graph from the start page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDir {
    /// Follow edges forward (source → destination): "everything this page
    /// rests on" — its causes/provenance.
    Up,
    /// Follow edges backward (destination → source): "everything derived
    /// from this page" — its consequences.
    Down,
}

/// The graph-query engine backing the `provenance_*` tools. The tool
/// surface is identical whichever arm answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphBackend {
    /// Stock DuckDB `WITH RECURSIVE` over `resolved_links`. Always
    /// available; no extension.
    RecursiveCte,
    /// DuckPGQ `MATCH` (community extension). Gated behind a load spike;
    /// not wired yet.
    #[allow(dead_code)]
    DuckPgq,
}

/// One node reached while walking the provenance graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceHop {
    /// `pages.page_id` of the reached node.
    pub page_id: String,
    /// The reached node's skill (entity type).
    pub skill: String,
    /// The relation KIND of the edge that reached it (e.g. `derived_from`);
    /// `None` for a bare/body link.
    pub relation: Option<String>,
    /// Hops from the start page (1 = a direct neighbour).
    pub depth: u32,
    /// The full page-id path start → node, used by the server to drop a
    /// hop whose path crosses an ACL-private node (fail-closed transitive
    /// visibility).
    pub path: Vec<String>,
}

/// One decision resting on a since-superseded expectation
/// (`expectation_drift`): the decision `motivated_by`/`addresses` an
/// expectation that a later revision `supersedes` — the cross-graph
/// "lost context" signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftRow {
    /// The decision whose motivating expectation drifted.
    pub decision_page_id: String,
    /// The decision's skill.
    pub decision_skill: String,
    /// The motivating expectation that was later superseded.
    pub expectation_page_id: String,
    /// The revision that superseded it.
    pub superseding_page_id: String,
    /// When the decision was made (`at`), as text.
    pub decided_at: String,
    /// When the superseding revision was authored (`at`), as text — later
    /// than `decided_at`, which is what makes the decision stale.
    pub superseded_at: String,
}

/// One node retired by supersession or abandonment (`abandoned_paths`):
/// something points at it via `supersedes` or `abandons`, so it is a
/// dead-ended branch of the project's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedNode {
    /// The retired node.
    pub page_id: String,
    /// Its skill.
    pub skill: String,
    /// How it was retired: `supersedes` or `abandons`.
    pub via: String,
}

/// A reachable path found by `provenance_path`: the ordered page-id path
/// from the start to the target (inclusive of both) and its hop count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenancePath {
    /// Ordered page ids, `from` first, `to` last.
    pub path: Vec<String>,
    /// Hops from start to target (`path.len() - 1`).
    pub depth: u32,
}

/// Unit separator — joins the path list into one scalar the `duckdb`
/// crate can read as a plain string, then split in Rust. Chosen because
/// it cannot occur in a page id.
const PATH_SEP: char = '\u{1f}';

impl Indexer {
    /// Which graph backend this indexer uses. Always `RecursiveCte` today;
    /// the DuckPGQ probe lands with the extension arm (ADR-0010 PR-6).
    #[must_use]
    pub fn graph_backend(&self) -> GraphBackend {
        GraphBackend::RecursiveCte
    }

    /// Bounded multi-hop ancestry from `page_id`.
    ///
    /// `direction = Up` returns everything `page_id` rests on (walks
    /// forward edges); `Down` returns everything derived from it (walks
    /// backward edges). `relations`, when non-empty, restricts the walk to
    /// those edge kinds — a path through a disallowed relation is cut.
    /// `max_hops` is clamped to `1..=MAX_HOPS_CEILING`. `as_of` gates every
    /// edge on its source page's birth (`src_at_ts <= as_of`), matching
    /// [`Indexer::neighbours`]; the walk is base-scenario only (v1).
    pub async fn provenance_ancestry(
        &self,
        page_id: &str,
        direction: GraphDir,
        relations: Option<&[String]>,
        max_hops: u32,
        as_of: Option<&str>,
    ) -> Result<Vec<ProvenanceHop>, IndexerError> {
        match self.graph_backend() {
            GraphBackend::RecursiveCte => {
                self.ancestry_cte(page_id, direction, relations, max_hops, as_of)
                    .await
            }
            GraphBackend::DuckPgq => {
                self.ancestry_cte(page_id, direction, relations, max_hops, as_of)
                    .await
            }
        }
    }

    /// The cross-graph "lost context" query: decisions resting on an
    /// expectation that has since been superseded.
    ///
    /// A decision `d --motivated_by|addresses--> x` where some `y
    /// --supersedes--> x` was authored *after* `d` (`y.at > d.at`) is
    /// **stale**: the reason it was made no longer holds. Pure
    /// `resolved_links` analytics — a two-join self-relation, no recursion,
    /// no extension. Base scenario only (v1). `skill`, when set, restricts
    /// to decisions of that skill.
    pub async fn expectation_drift(
        &self,
        skill: Option<&str>,
    ) -> Result<Vec<DriftRow>, IndexerError> {
        let mut sql = String::from(
            "SELECT x.src_page_id, x.src_skill, x.dst_page_id, s.src_page_id, \
                    CAST(x.src_at_ts AS VARCHAR), CAST(s.src_at_ts AS VARCHAR) \
             FROM resolved_links x \
             JOIN resolved_links s ON s.dst_page_id = x.dst_page_id \
             WHERE x.relation IN ('motivated_by', 'addresses') \
               AND s.relation = 'supersedes' \
               AND x.src_at_ts IS NOT NULL AND s.src_at_ts IS NOT NULL \
               AND s.src_at_ts > x.src_at_ts \
               AND x.src_scenario IS NULL AND s.src_scenario IS NULL",
        );
        let mut binds: Vec<String> = Vec::new();
        if let Some(sk) = skill {
            sql.push_str(" AND x.src_skill = ?");
            binds.push(sk.to_owned());
        }
        sql.push_str(" ORDER BY x.src_page_id, s.src_page_id");

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), |row| {
            Ok(DriftRow {
                decision_page_id: row.get(0)?,
                decision_skill: row.get(1)?,
                expectation_page_id: row.get(2)?,
                superseding_page_id: row.get(3)?,
                decided_at: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                superseded_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Nodes retired by supersession or abandonment — the dead-ended
    /// branches of the memory. A node is returned when something points at
    /// it via `supersedes` (a newer revision replaced it) or `abandons` (a
    /// decision dropped it). Pure `resolved_links` analytics; base scenario
    /// only. `skill`, when set, restricts to retired nodes of that skill.
    pub async fn abandoned_paths(
        &self,
        skill: Option<&str>,
    ) -> Result<Vec<AbandonedNode>, IndexerError> {
        let mut sql = String::from(
            "SELECT DISTINCT r.dst_page_id, r.dst_skill, r.relation \
             FROM resolved_links r \
             WHERE r.relation IN ('supersedes', 'abandons') \
               AND r.src_scenario IS NULL",
        );
        let mut binds: Vec<String> = Vec::new();
        if let Some(sk) = skill {
            sql.push_str(" AND r.dst_skill = ?");
            binds.push(sk.to_owned());
        }
        sql.push_str(" ORDER BY r.dst_page_id");

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), |row| {
            Ok(AbandonedNode {
                page_id: row.get(0)?,
                skill: row.get(1)?,
                via: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Shortest provenance path from `from_page` to `to_page` within
    /// `max_hops`, following `direction` (`Up` = the rests-on chain), or
    /// `None` if unreachable. Reuses the bounded ancestry walk and keeps
    /// the shallowest path that reaches the target.
    pub async fn provenance_path(
        &self,
        from_page: &str,
        to_page: &str,
        direction: GraphDir,
        relations: Option<&[String]>,
        max_hops: u32,
    ) -> Result<Option<ProvenancePath>, IndexerError> {
        let hops = self
            .provenance_ancestry(from_page, direction, relations, max_hops, None)
            .await?;
        Ok(hops
            .into_iter()
            .filter(|h| h.page_id == to_page)
            .min_by_key(|h| h.depth)
            .map(|h| ProvenancePath {
                path: h.path,
                depth: h.depth,
            }))
    }

    /// Recursive-CTE implementation of [`Indexer::provenance_ancestry`].
    async fn ancestry_cte(
        &self,
        page_id: &str,
        direction: GraphDir,
        relations: Option<&[String]>,
        max_hops: u32,
        as_of: Option<&str>,
    ) -> Result<Vec<ProvenanceHop>, IndexerError> {
        let hops = max_hops.clamp(1, MAX_HOPS_CEILING);

        // Direction picks which endpoint is the "from" (matched in the
        // WHERE / joined on) and which is the "to" (the node we surface).
        let (from_col, to_col, to_skill) = match direction {
            GraphDir::Up => ("src_page_id", "dst_page_id", "dst_skill"),
            GraphDir::Down => ("dst_page_id", "src_page_id", "src_skill"),
        };

        // Relation filter appears in BOTH the anchor and the recursive step,
        // so its binds are pushed twice, in query order.
        let rel_clause = match relations {
            Some(rs) if !rs.is_empty() => {
                let ph = std::iter::repeat_n("?", rs.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" AND r.relation IN ({ph})")
            }
            _ => String::new(),
        };
        // as_of clause (source-birth gate), likewise in both steps.
        let asof_clause = if as_of.is_some() {
            " AND (r.src_at_ts <= ? OR r.src_at_ts IS NULL)"
        } else {
            ""
        };

        let sql = format!(
            "WITH RECURSIVE walk(node, skill, relation, depth, path) AS ( \
                 SELECT r.{to_col}, r.{to_skill}, r.relation, 1, [r.{from_col}, r.{to_col}] \
                 FROM resolved_links r \
                 WHERE r.{from_col} = ? AND r.src_scenario IS NULL{rel_clause}{asof_clause} \
               UNION ALL \
                 SELECT r.{to_col}, r.{to_skill}, r.relation, w.depth + 1, \
                        list_append(w.path, r.{to_col}) \
                 FROM resolved_links r JOIN walk w ON r.{from_col} = w.node \
                 WHERE w.depth < {hops} AND r.src_scenario IS NULL{rel_clause}{asof_clause} \
                   AND NOT list_contains(w.path, r.{to_col}) \
             ) \
             SELECT node, skill, relation, depth, array_to_string(path, chr(31)) \
             FROM walk ORDER BY depth, node"
        );

        // Binds in query order: anchor (start, [relations], [as_of]) then
        // recursive step ([relations], [as_of]).
        let mut binds: Vec<String> = vec![page_id.to_owned()];
        if let Some(rs) = relations {
            binds.extend(rs.iter().cloned());
        }
        if let Some(ts) = as_of {
            binds.push(ts.to_owned());
        }
        if let Some(rs) = relations {
            binds.extend(rs.iter().cloned());
        }
        if let Some(ts) = as_of {
            binds.push(ts.to_owned());
        }

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), |row| {
            let relation: Option<String> = row.get(2)?;
            let depth: i64 = row.get(3)?;
            let path_str: String = row.get(4)?;
            Ok(ProvenanceHop {
                page_id: row.get(0)?,
                skill: row.get(1)?,
                relation,
                depth: u32::try_from(depth).unwrap_or(0),
                path: path_str.split(PATH_SEP).map(str::to_owned).collect(),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
