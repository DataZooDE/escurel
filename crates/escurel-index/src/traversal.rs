//! Stored corpus traversals (#511) — a `[[query::*]]` page whose `target` is
//! the **corpus** rather than a `sql_view` instance.
//!
//! The gap this closes: `query_instance` reads EXTERNAL tables through a
//! managed `vw_…` view, never the markdown corpus, and the legacy
//! `run_stored_query` — pre-declared arbitrary SQL over `pages` — was removed
//! in the 2026-08-14 consolidation for a reason that has not gone away:
//! arbitrary SQL has no per-row owner to ACL against. So a question like
//! *"which of our people knows someone at this account"* could not be saved
//! as a named, parameterised, reviewable artefact, and had to be assembled
//! client-side out of N `neighbours` calls — slow, unatomic, and invisible to
//! ACL reasoning as a whole.
//!
//! What this is, and deliberately is not:
//!
//! - **Not SQL, and not a graph query language.** A `.gq` / `MATCH` surface
//!   re-opens exactly the hole the consolidation closed. A traversal declares
//!   a fixed chain of typed hops; the caller supplies VALUES, never syntax.
//! - **Bounded by construction.** The steps are enumerated in the page, so
//!   the path length is known before anything runs; `max_depth` is mandatory
//!   and capped server-side, and a path never revisits a page.
//! - **ACL per traversed row, fail-closed.** Every instance on a path is run
//!   through `may_read_instance`; one unreadable hop drops the whole path.
//!   A caller sees exactly what they could have reached by walking
//!   `neighbours` themselves — which is the property `run_stored_query` could
//!   not provide, and the whole reason this is declarative.
//! - **A page.** Versioned, reviewable, ACL'd, shippable in a skill pack, and
//!   lintable like anything else in the corpus.
//!
//! `relation:` is the frontmatter key a link came FROM — `links.src_field`,
//! already indexed and exposed by the `resolved_links` view the provenance
//! tools walk. No new storage.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::indexer::{Indexer, IndexerError};

/// The `target:` value that selects this path instead of a `sql_view`.
pub const CORPUS_TARGET: &str = "corpus";

/// Hard ceiling on a declared `max_depth`, whatever the page asks for.
///
/// Mirrors [`crate::graph::MAX_HOPS_CEILING`] — the provenance tools' cap —
/// because this is the same walk over the same view, and two different
/// ceilings on one graph is a difference nobody can explain later.
pub const MAX_DEPTH_CEILING: u32 = 12;

/// Which way a hop runs along a link.
///
/// `out` follows the link as written (the page that DECLARES the key → the
/// page it names). `in` walks it backwards (who points AT me), which is the
/// direction almost every interesting question takes: a company does not
/// declare its employees, people declare their employer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Out,
    In,
}

impl Dir {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "out" | "outbound" | "forward" => Some(Dir::Out),
            "in" | "inbound" | "reverse" => Some(Dir::In),
            _ => None,
        }
    }
}

/// One hop: follow `relation` in `direction`, and call what you land on
/// `alias`.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub relation: String,
    pub direction: Dir,
    pub alias: String,
}

/// Where the walk begins: an instance of `skill` whose id is `id` — which may
/// be a `{{param}}` placeholder, substituted with a caller VALUE.
#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    pub skill: String,
    pub id: String,
}

/// An equality filter on a reached instance's frontmatter, written
/// `alias.key`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub alias: String,
    pub key: String,
    pub equals: String,
}

/// A declared traversal, parsed from a query page's `traversal:` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Traversal {
    pub start: Start,
    pub steps: Vec<Step>,
    pub filters: Vec<Filter>,
    /// Projected columns, each `alias.key`. `alias.id` and `alias.page_id`
    /// are always available; everything else comes from the instance's
    /// frontmatter.
    pub returns: Vec<String>,
    pub max_depth: u32,
    pub limit: usize,
}

/// Why a `traversal:` block could not be used. Each maps to one validation
/// issue code, so `validate` can report the author's mistake precisely.
#[derive(Debug, Clone, PartialEq)]
pub enum TraversalError {
    /// Structurally unusable: not a mapping, no `start`, a step with no
    /// `relation`/`as`, an unreadable `direction`.
    Malformed(String),
    /// `max_depth` absent, zero, past [`MAX_DEPTH_CEILING`], or shorter than
    /// the steps the page declares.
    Depth(String),
    /// A `return:`/`where:` entry naming an alias no step declares, or a
    /// column with no `alias.key` shape.
    UnknownField(String),
}

impl TraversalError {
    /// The validation issue code this maps to.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            TraversalError::Malformed(_) => "traversal_malformed",
            TraversalError::Depth(_) => "traversal_depth_exceeded",
            TraversalError::UnknownField(_) => "traversal_unknown_field",
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            TraversalError::Malformed(m)
            | TraversalError::Depth(m)
            | TraversalError::UnknownField(m) => m,
        }
    }
}

/// The alias of the START node, so `where:`/`return:` can name it without the
/// page having to invent one.
pub const START_ALIAS: &str = "start";

/// Parse a `traversal:` block from a query page's frontmatter.
///
/// Returns `Ok(None)` when the page declares no block at all — that is a
/// `sql_view` query and none of this applies.
///
/// # Errors
/// When the block is present and unusable; every variant maps to one
/// validation issue code.
pub fn parse_traversal(fm: &Value) -> Result<Option<Traversal>, TraversalError> {
    let Some(raw) = fm.get("traversal") else {
        return Ok(None);
    };
    let obj = raw.as_object().ok_or_else(|| {
        TraversalError::Malformed(
            "`traversal:` must be a mapping with `start`, `steps`, `return` and `max_depth`"
                .to_owned(),
        )
    })?;

    // ── start ──
    let start_raw = obj.get("start").and_then(Value::as_object).ok_or_else(|| {
        TraversalError::Malformed(
            "`traversal.start` must be a mapping naming the skill and id to begin at, \
             e.g. `{skill: company, id: \"{{account}}\"}`"
                .to_owned(),
        )
    })?;
    let text = |m: &serde_json::Map<String, Value>, k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let start = Start {
        skill: text(start_raw, "skill").ok_or_else(|| {
            TraversalError::Malformed("`traversal.start.skill` is required".to_owned())
        })?,
        id: text(start_raw, "id").ok_or_else(|| {
            TraversalError::Malformed(
                "`traversal.start.id` is required — a literal id, or a `{{param}}` \
                 placeholder the caller supplies"
                    .to_owned(),
            )
        })?,
    };

    // ── steps ──
    let steps_raw = obj.get("steps").and_then(Value::as_array).ok_or_else(|| {
        TraversalError::Malformed(
            "`traversal.steps` must be a sequence of `{relation, direction, as}` hops".to_owned(),
        )
    })?;
    if steps_raw.is_empty() {
        return Err(TraversalError::Malformed(
            "`traversal.steps` is empty — a traversal with no hops is the start node, \
             which `expand` already returns"
                .to_owned(),
        ));
    }
    let mut steps = Vec::new();
    let mut aliases: HashSet<String> = HashSet::from([START_ALIAS.to_owned()]);
    for (i, step) in steps_raw.iter().enumerate() {
        let m = step.as_object().ok_or_else(|| {
            TraversalError::Malformed(format!("`traversal.steps[{i}]` must be a mapping"))
        })?;
        let relation = text(m, "relation").ok_or_else(|| {
            TraversalError::Malformed(format!(
                "`traversal.steps[{i}].relation` is required — it is the frontmatter \
                 key the link was written under"
            ))
        })?;
        let direction = match text(m, "direction") {
            // Unset means `out`: following a link as written is the reading
            // that needs no explanation.
            None => Dir::Out,
            Some(d) => Dir::parse(&d).ok_or_else(|| {
                TraversalError::Malformed(format!(
                    "`traversal.steps[{i}].direction: {d}` is neither `in` nor `out`"
                ))
            })?,
        };
        let alias = text(m, "as").ok_or_else(|| {
            TraversalError::Malformed(format!(
                "`traversal.steps[{i}].as` is required — `return:` and `where:` name \
                 what a hop landed on by its alias"
            ))
        })?;
        if !aliases.insert(alias.clone()) {
            return Err(TraversalError::Malformed(format!(
                "alias `{alias}` is used by two steps; `return: {alias}.…` could then \
                 mean either hop"
            )));
        }
        steps.push(Step {
            relation,
            direction,
            alias,
        });
    }

    // ── depth ──
    let max_depth = obj
        .get("max_depth")
        .and_then(Value::as_u64)
        .and_then(|d| u32::try_from(d).ok())
        .ok_or_else(|| {
            TraversalError::Depth(
                "`traversal.max_depth` is required — an unbounded walk over the corpus \
                 is exactly what this surface exists not to offer"
                    .to_owned(),
            )
        })?;
    if max_depth == 0 || max_depth > MAX_DEPTH_CEILING {
        return Err(TraversalError::Depth(format!(
            "`traversal.max_depth: {max_depth}` is outside 1..={MAX_DEPTH_CEILING}"
        )));
    }
    let declared = u32::try_from(steps.len()).unwrap_or(u32::MAX);
    if declared > max_depth {
        return Err(TraversalError::Depth(format!(
            "`traversal` declares {declared} steps but `max_depth: {max_depth}` — the \
             walk could never finish, so it would always return nothing"
        )));
    }

    // ── where / return ──
    let split = |col: &str| -> Result<(String, String), TraversalError> {
        let (alias, key) = col.split_once('.').ok_or_else(|| {
            TraversalError::UnknownField(format!(
                "`{col}` must be written `<alias>.<frontmatter key>`"
            ))
        })?;
        if !aliases.contains(alias) {
            return Err(TraversalError::UnknownField(format!(
                "`{col}` names alias `{alias}`, which no step declares"
            )));
        }
        Ok((alias.to_owned(), key.to_owned()))
    };

    let mut filters = Vec::new();
    for w in obj
        .get("where")
        .and_then(Value::as_array)
        .unwrap_or(&vec![])
    {
        let m = w.as_object().ok_or_else(|| {
            TraversalError::Malformed(
                "`traversal.where` entries must be `{field, equals}` mappings".to_owned(),
            )
        })?;
        let field = text(m, "field").ok_or_else(|| {
            TraversalError::Malformed("a `traversal.where` entry needs a `field`".to_owned())
        })?;
        let equals = m
            .get("equals")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .ok_or_else(|| {
                TraversalError::Malformed(format!(
                    "`traversal.where` on `{field}` needs an `equals:` — it is the only \
                     comparison this surface offers"
                ))
            })?;
        let (alias, key) = split(&field)?;
        filters.push(Filter { alias, key, equals });
    }

    let returns_raw = obj.get("return").and_then(Value::as_array).ok_or_else(|| {
        TraversalError::Malformed(
            "`traversal.return` must list the columns to project, e.g. \
             `[teammate.name, contact.name]`"
                .to_owned(),
        )
    })?;
    let mut returns = Vec::new();
    for col in returns_raw {
        let col = col.as_str().ok_or_else(|| {
            TraversalError::UnknownField("`traversal.return` entries must be strings".to_owned())
        })?;
        split(col)?;
        returns.push(col.to_owned());
    }
    if returns.is_empty() {
        return Err(TraversalError::Malformed(
            "`traversal.return` is empty — a query that projects nothing has no answer".to_owned(),
        ));
    }

    let limit = obj
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|l| usize::try_from(l).ok())
        .filter(|l| *l > 0)
        .unwrap_or(crate::query::MAX_RESULT_ROWS)
        .min(crate::query::MAX_RESULT_ROWS);

    Ok(Some(Traversal {
        start,
        steps,
        filters,
        returns,
        max_depth,
        limit,
    }))
}

/// One reached node, mid-walk.
#[derive(Debug, Clone)]
struct Reached {
    /// `alias` → the page reached under it, including [`START_ALIAS`].
    bound: HashMap<String, String>,
    /// Every page on this path, so a walk never revisits one.
    seen: HashSet<String>,
}

impl Indexer {
    /// Run a declared traversal for `caller`, returning its projected rows.
    ///
    /// The walk is step-wise rather than one recursive CTE, because the steps
    /// are ENUMERATED: each hop has a fixed relation and direction, so there
    /// is no recursion to express — and doing it a hop at a time is what lets
    /// the ACL filter run on the instances actually traversed, which a single
    /// SQL statement could not do without teaching SQL the ACL rules.
    ///
    /// Every hop binds its parameters; no caller text is ever interpolated
    /// into a statement.
    ///
    /// # Errors
    /// When the underlying queries fail.
    pub async fn run_traversal(
        &self,
        traversal: &Traversal,
        start_id: &str,
        scenario: Option<&str>,
        caller: &crate::AclCaller<'_>,
    ) -> Result<(Vec<serde_json::Map<String, Value>>, bool), IndexerError> {
        // The start node, resolved like any other instance — and ACL'd, so a
        // traversal cannot confirm the existence of a record the caller may
        // not read by returning "no rows" for one id and an error for another.
        let start_page = self
            .instance_page_id(&traversal.start.skill, start_id, scenario)
            .await?;
        let Some(start_page) = start_page else {
            return Ok((Vec::new(), false));
        };
        if !self.may_read_page(&start_page, caller).await? {
            return Ok((Vec::new(), false));
        }

        let mut frontier = vec![Reached {
            bound: HashMap::from([(START_ALIAS.to_owned(), start_page.clone())]),
            seen: HashSet::from([start_page]),
        }];

        for step in &traversal.steps {
            let mut next = Vec::new();
            for reached in &frontier {
                let from = reached
                    .bound
                    .get(&prev_alias(traversal, step))
                    .expect("every step's predecessor is bound");
                for landed in self
                    .hop(from, &step.relation, step.direction, scenario)
                    .await?
                {
                    // A path never revisits a page: without this a `knows`
                    // relation walks A → B → A for ever, and the cycle is the
                    // ordinary case in a corpus about people.
                    if reached.seen.contains(&landed) {
                        continue;
                    }
                    // Fail closed, per hop: one unreadable instance drops the
                    // whole path rather than the row's contribution, because a
                    // path is only evidence if every step of it is visible.
                    if !self.may_read_page(&landed, caller).await? {
                        continue;
                    }
                    let mut bound = reached.bound.clone();
                    bound.insert(step.alias.clone(), landed.clone());
                    let mut seen = reached.seen.clone();
                    seen.insert(landed);
                    next.push(Reached { bound, seen });
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        // ── project + filter ──
        let mut rows = Vec::new();
        let mut truncated = false;
        for reached in frontier {
            let mut fm_cache: HashMap<&String, Value> = HashMap::new();
            for page in reached.bound.values() {
                if !fm_cache.contains_key(page) {
                    let fm = self.page_frontmatter_value(page).await?;
                    fm_cache.insert(page, fm);
                }
            }
            let value_of = |alias: &str, key: &str| -> Option<Value> {
                let page = reached.bound.get(alias)?;
                if key == "page_id" {
                    return Some(Value::String(page.clone()));
                }
                fm_cache.get(page).and_then(|fm| fm.get(key).cloned())
            };
            // A filter compares the author's TEXT against the value as the
            // author would have written it: a string to itself, anything else
            // through its JSON spelling (`12`, `true`). Frontmatter is untyped
            // today, so `seats: 12` and `seats: "12"` are the same fact to a
            // reader and must be the same fact here.
            if !traversal.filters.iter().all(|f| {
                value_of(&f.alias, &f.key).is_some_and(|v| match &v {
                    Value::String(s) => s == &f.equals,
                    other => {
                        let spelled = other.to_string();
                        spelled == f.equals
                    }
                })
            }) {
                continue;
            }
            let mut row = serde_json::Map::new();
            for col in &traversal.returns {
                let (alias, key) = col.split_once('.').unwrap_or((col.as_str(), ""));
                row.insert(col.clone(), value_of(alias, key).unwrap_or(Value::Null));
            }
            if rows.len() >= traversal.limit {
                truncated = true;
                break;
            }
            rows.push(row);
        }
        Ok((rows, truncated))
    }

    /// The pages reachable from `page_id` in one hop along `relation`.
    ///
    /// Reads `resolved_links` — the same view the provenance tools walk — so
    /// `relation` is `links.src_field`, the frontmatter key the link was
    /// written under. Both endpoints are bound parameters.
    ///
    /// Scenario-aware (#512 §5), with the override the read tools already
    /// apply: with no scenario only the base timeline is walked; with one, a
    /// landed page whose slug has an overlay resolves to the OVERLAY, so a
    /// branch read never returns a slug twice. A query surface that ignored
    /// this would silently double-count the moment an overlay existed, which
    /// is the failure nobody notices because the number still looks like a
    /// number.
    async fn hop(
        &self,
        page_id: &str,
        relation: &str,
        direction: Dir,
        scenario: Option<&str>,
    ) -> Result<Vec<String>, IndexerError> {
        let (from_col, to_col) = match direction {
            Dir::Out => ("src_page_id", "dst_page_id"),
            Dir::In => ("dst_page_id", "src_page_id"),
        };
        // Edges are gated by their SOURCE page's scenario, mirroring how
        // `neighbours` gates them.
        let edge_gate = if scenario.is_some() {
            "(r.src_scenario = ? OR r.src_scenario IS NULL)"
        } else {
            "r.src_scenario IS NULL"
        };
        // Then the per-slug override: `ORDER BY scenario NULLS LAST` is the
        // crux — the non-null overlay row sorts FIRST and is kept, the base
        // twin dropped. Flip it and the overlay is silently ignored, with no
        // type error to catch it (docs/notes/discovered/
        // 2026-05-29-scenario-overlay-qualify.md).
        let sql = if scenario.is_some() {
            format!(
                "WITH landed AS ( \
                     SELECT DISTINCT d.skill, d.slug \
                     FROM resolved_links r JOIN pages d ON d.page_id = r.{to_col} \
                     WHERE r.{from_col} = ? AND r.relation = ? AND {edge_gate} \
                 ) \
                 SELECT p.page_id FROM pages p JOIN landed l \
                   ON p.skill = l.skill AND p.slug = l.slug \
                 WHERE (p.scenario = ? OR p.scenario IS NULL) \
                 QUALIFY ROW_NUMBER() OVER ( \
                     PARTITION BY p.slug ORDER BY p.scenario NULLS LAST, p.page_id) = 1"
            )
        } else {
            format!(
                "SELECT DISTINCT r.{to_col} FROM resolved_links r \
                 WHERE r.{from_col} = ? AND r.relation = ? AND {edge_gate}"
            )
        };
        let mut binds: Vec<String> = vec![page_id.to_owned(), relation.to_owned()];
        if let Some(sc) = scenario {
            // Once for the edge gate, once for the page override.
            binds.push(sc.to_owned());
            binds.push(sc.to_owned());
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// `page_id` of an instance, by skill + id — under `scenario` when one is
    /// given, with the same per-slug override every read path applies.
    async fn instance_page_id(
        &self,
        skill: &str,
        id: &str,
        scenario: Option<&str>,
    ) -> Result<Option<String>, IndexerError> {
        let conn = self.conn.lock().await;
        let (sql, binds): (String, Vec<String>) = match scenario {
            Some(sc) => (
                "SELECT page_id FROM pages \
                 WHERE page_type = 'instance' AND skill = ? AND slug = ? \
                   AND (scenario = ? OR scenario IS NULL) \
                 ORDER BY scenario NULLS LAST, page_id LIMIT 1"
                    .to_owned(),
                vec![skill.to_owned(), id.to_owned(), sc.to_owned()],
            ),
            None => (
                "SELECT page_id FROM pages \
                 WHERE page_type = 'instance' AND skill = ? AND slug = ? AND scenario IS NULL \
                 LIMIT 1"
                    .to_owned(),
                vec![skill.to_owned(), id.to_owned()],
            ),
        };
        Ok(conn
            .query_row(&sql, duckdb::params_from_iter(binds.iter()), |row| {
                row.get::<_, String>(0)
            })
            .ok())
    }

    /// A page's frontmatter as JSON (`{}` when it has none).
    async fn page_frontmatter_value(&self, page_id: &str) -> Result<Value, IndexerError> {
        let conn = self.conn.lock().await;
        let raw: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages WHERE page_id = ?",
                duckdb::params![page_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        drop(conn);
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({})))
    }

    /// Whether `caller` may read the instance at `page_id` — the same
    /// fail-closed decision every read tool makes, asked per traversed hop.
    async fn may_read_page(
        &self,
        page_id: &str,
        caller: &crate::AclCaller<'_>,
    ) -> Result<bool, IndexerError> {
        let skill: Option<String> = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT skill FROM pages WHERE page_id = ?",
                duckdb::params![page_id],
                |row| row.get(0),
            )
            .ok()
        };
        let Some(skill) = skill else {
            return Ok(false);
        };
        let fm = self.page_frontmatter_value(page_id).await?;
        self.may_read_instance(caller, &skill, &fm).await
    }
}

/// The alias a step starts FROM: the previous step's alias, or the start.
fn prev_alias(traversal: &Traversal, step: &Step) -> String {
    match traversal.steps.iter().position(|s| s.alias == step.alias) {
        Some(0) | None => START_ALIAS.to_owned(),
        Some(i) => traversal.steps[i - 1].alias.clone(),
    }
}

/// Test-only helper: `Result::unwrap_err` with a caller-supplied message.
#[cfg(test)]
trait UnwrapErrOr<E> {
    fn unwrap_err_or_else(self, f: impl FnOnce() -> E) -> E;
}

#[cfg(test)]
impl<T, E> UnwrapErrOr<E> for Result<T, E> {
    fn unwrap_err_or_else(self, f: impl FnOnce() -> E) -> E {
        match self {
            Err(e) => e,
            Ok(_) => f(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(v: Value) -> Value {
        json!({ "traversal": v })
    }

    #[test]
    fn the_warm_intro_page_parses_into_the_walk_it_describes() {
        let t = parse_traversal(&block(json!({
            "start": {"skill": "company", "id": "{{account}}"},
            "steps": [
                {"relation": "works_at", "direction": "in", "as": "contact"},
                {"relation": "knows", "direction": "in", "as": "teammate"},
            ],
            "where": [{"field": "teammate.employer", "equals": "com-datazoo"}],
            "return": ["teammate.name", "contact.name"],
            "max_depth": 3,
            "limit": 200,
        })))
        .expect("parses")
        .expect("a block is present");

        assert_eq!(t.start.skill, "company");
        assert_eq!(t.start.id, "{{account}}");
        assert_eq!(t.steps.len(), 2);
        assert_eq!(t.steps[0].direction, Dir::In);
        assert_eq!(t.steps[1].alias, "teammate");
        assert_eq!(t.filters[0].alias, "teammate");
        assert_eq!(t.filters[0].key, "employer");
        assert_eq!(t.returns, vec!["teammate.name", "contact.name"]);
        assert_eq!(t.limit, 200);
    }

    #[test]
    fn a_page_with_no_traversal_block_is_not_a_traversal() {
        assert_eq!(parse_traversal(&json!({"sql": "SELECT 1"})), Ok(None));
    }

    #[test]
    fn every_way_a_bound_can_be_missing_is_refused() {
        let base = json!({
            "start": {"skill": "company", "id": "globex"},
            "steps": [{"relation": "works_at", "direction": "in", "as": "c"}],
            "return": ["c.name"],
        });
        let with = |k: &str, v: Value| {
            let mut b = base.clone();
            b[k] = v;
            block(b)
        };

        // No `max_depth` at all.
        assert_eq!(
            parse_traversal(&block(base.clone())).unwrap_err().code(),
            "traversal_depth_exceeded"
        );
        // Zero, and past the ceiling.
        for d in [json!(0), json!(MAX_DEPTH_CEILING + 1), json!(999)] {
            assert_eq!(
                parse_traversal(&with("max_depth", d)).unwrap_err().code(),
                "traversal_depth_exceeded"
            );
        }
        // A depth that cannot fit the declared steps is the same bug seen
        // from the other side, and would silently return nothing.
        let mut deep = base.clone();
        deep["steps"] = json!([
            {"relation": "a", "as": "x"},
            {"relation": "b", "as": "y"},
        ]);
        deep["max_depth"] = json!(1);
        assert_eq!(
            parse_traversal(&block(deep)).unwrap_err().code(),
            "traversal_depth_exceeded"
        );
    }

    #[test]
    fn a_structurally_unusable_block_names_what_is_wrong() {
        let cases = [
            (json!("corpus"), "traversal_malformed"),
            (
                json!({"steps": [], "return": ["c.n"], "max_depth": 2}),
                "traversal_malformed",
            ),
            (
                json!({"start": {"skill": "company", "id": "g"}, "steps": [{"as": "c"}],
                       "return": ["c.n"], "max_depth": 2}),
                "traversal_malformed",
            ),
            (
                json!({"start": {"skill": "company", "id": "g"},
                       "steps": [{"relation": "works_at", "direction": "sideways", "as": "c"}],
                       "return": ["c.n"], "max_depth": 2}),
                "traversal_malformed",
            ),
            // Two steps sharing one alias: `return: c.name` could mean either.
            (
                json!({"start": {"skill": "company", "id": "g"},
                       "steps": [{"relation": "a", "as": "c"}, {"relation": "b", "as": "c"}],
                       "return": ["c.n"], "max_depth": 3}),
                "traversal_malformed",
            ),
            // A return naming an alias no step declares can only be empty.
            (
                json!({"start": {"skill": "company", "id": "g"},
                       "steps": [{"relation": "a", "as": "c"}],
                       "return": ["nobody.name"], "max_depth": 2}),
                "traversal_unknown_field",
            ),
            (
                json!({"start": {"skill": "company", "id": "g"},
                       "steps": [{"relation": "a", "as": "c"}],
                       "return": ["bare"], "max_depth": 2}),
                "traversal_unknown_field",
            ),
        ];
        for (raw, expected) in cases {
            let err = parse_traversal(&block(raw.clone()))
                .unwrap_err_or_else(|| panic!("{raw} must be refused"));
            assert_eq!(err.code(), expected, "{raw}: {err:?}");
        }
    }

    /// The start alias needs no declaration — `return: start.name` works
    /// without the page inventing a step for the node it began at.
    #[test]
    fn the_start_node_can_be_projected() {
        let t = parse_traversal(&block(json!({
            "start": {"skill": "company", "id": "globex"},
            "steps": [{"relation": "works_at", "direction": "in", "as": "c"}],
            "return": ["start.name", "c.name"],
            "max_depth": 2,
        })))
        .expect("parses")
        .expect("present");
        assert_eq!(t.returns, vec!["start.name", "c.name"]);
    }

    /// A declared `limit` is capped by the server's own backstop, so a page
    /// cannot ask for an unbounded result set.
    #[test]
    fn a_declared_limit_cannot_exceed_the_servers_cap() {
        let t = parse_traversal(&block(json!({
            "start": {"skill": "company", "id": "globex"},
            "steps": [{"relation": "works_at", "as": "c"}],
            "return": ["c.name"],
            "max_depth": 2,
            "limit": 10_000_000,
        })))
        .expect("parses")
        .expect("present");
        assert_eq!(t.limit, crate::query::MAX_RESULT_ROWS);
    }
}
