//! Held writes — a finished change that has not landed, waiting for a human.
//!
//! A skill page may declare `autonomy: review`, and `list_skills` publishes
//! that declaration, but until now nothing could act on it: an agent could
//! write or not write, and there was nowhere to put a change that is complete
//! and not yet wanted. Consumers filled the gap privately — heron modelled a
//! pending change as an ordinary instance of its own `proposal` skill — which
//! put a consumer-shaped object in the knowledge base and made "what is
//! waiting for me?" a question only that consumer could answer.
//!
//! **A draft is not a page.** It is never returned by `expand`, `search`,
//! `list_instances` or `neighbours`, so an unapproved change cannot be
//! mistaken for knowledge by a reader, by an agent gathering context, or by
//! the runner's cascade. Storing it outside `pages` makes that true by
//! construction instead of by an exclusion rule in every read path — the kind
//! of rule that is eventually forgotten in one of them.
//!
//! **Immutable after creation.** A revised draft is a new row. A human
//! approves specific bytes, and bytes that can change under an approval turn
//! the gate into decoration; [`DraftInfo::content_sha256`] is what promotion
//! compares against.
//!
//! Backed by the same three-variant storage the events surface uses, for the
//! same reason: a deployment whose replicas share one catalog must not keep a
//! draft in a file only one of them can see. `heron-escurel` runs with no
//! persistent local volume at all, so a `Local`-only draft would be lost on
//! every rollout — and a review queue that empties itself on deploy is worse
//! than no review queue, because nobody notices.

use crate::indexer::{Indexer, IndexerError};
use ulid::Ulid;

/// Table name for the shared attached-Postgres drafts table. Lives here —
/// `drafts.rs` owns the drafts concept — and `snapshot::drafts_pg` imports it
/// back for the `CREATE TABLE` DDL so the name is defined exactly once.
/// Mirrors [`crate::events::EVENTS_PG_TABLE_NAME`].
pub const DRAFTS_PG_TABLE_NAME: &str = "escurel_drafts";

/// Which physical table the drafts methods read and write. Mirrors
/// [`crate::events::EventsBackend`] exactly; see that type for why the
/// attached variants carry an explicit `tenant` column and the local one
/// does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftsBackend {
    /// The local per-tenant `drafts` table.
    Local,
    /// A read-write table in the lake.
    AttachedLake {
        /// The DuckDB `ATTACH` alias.
        alias: String,
    },
    /// An attached, read-write Postgres table shared by every replica.
    AttachedPostgres {
        /// The DuckDB `ATTACH` alias.
        alias: String,
    },
}

/// A held write, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftInfo {
    /// Server-generated ULID.
    pub draft_id: String,
    /// The page this write is for. It need not exist yet — a draft that
    /// creates a page is the ordinary case for a capture being filed.
    pub target_page_id: String,
    /// The whole proposed markdown, frontmatter first.
    pub content: String,
    /// Hex sha256 of [`Self::content`] — the byte binding an approval is
    /// made against.
    pub content_sha256: String,
    /// The target's `content_sha256` when this was drafted; `None` when it
    /// was drafted as a create. Carried into the page CAS on promotion so a
    /// target that moved underneath conflicts instead of being clobbered.
    pub base_sha256: Option<String>,
    /// The subject that drafted it — an agent, normally.
    pub author: String,
    /// The inbox event this answers, when it answers one.
    pub event_id: Option<String>,
    /// `open` | `promoted` | `discarded`.
    pub status: String,
    /// Why it was discarded, when it was.
    pub reason: String,
    /// The subject that promoted or discarded it.
    pub decided_by: String,
    /// RFC 3339.
    pub created_at: String,
}

/// Input to [`Indexer::create_draft`].
#[derive(Debug, Clone, Default)]
pub struct NewDraft {
    /// The page this write is for.
    pub target_page_id: String,
    /// The whole proposed markdown.
    pub content: String,
    /// The target's hash at drafting time; `None` for a create.
    pub base_sha256: Option<String>,
    /// The drafting subject.
    pub author: String,
    /// The inbox event this answers, if any.
    pub event_id: Option<String>,
}

/// Hex sha256 of a draft's bytes. Free function so the server can compute the
/// same value when checking a promotion's `draft_sha256` without holding an
/// `Indexer`.
#[must_use]
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

fn row_to_draft(row: &duckdb::Row<'_>) -> duckdb::Result<DraftInfo> {
    Ok(DraftInfo {
        draft_id: row.get(0)?,
        target_page_id: row.get(1)?,
        content: row.get(2)?,
        content_sha256: row.get(3)?,
        base_sha256: row.get(4)?,
        author: row.get(5)?,
        event_id: row.get(6)?,
        status: row.get(7)?,
        reason: row.get(8)?,
        decided_by: row.get(9)?,
        created_at: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
    })
}

fn select_cols(table: &str) -> String {
    format!(
        "SELECT draft_id, target_page_id, content, content_sha256, base_sha256, \
         author, event_id, status, reason, decided_by, \
         strftime(created_at, '%Y-%m-%dT%H:%M:%SZ') \
         FROM {table}"
    )
}

impl Indexer {
    /// True when drafts live in a table every replica can reach — the gate a
    /// reader replica checks before serving the draft tools. Mirrors
    /// [`Indexer::has_shared_events`], including its lesson: match BOTH
    /// shared variants, or a DuckLake deployment wrongly rejects its own
    /// draft surface.
    #[must_use]
    pub fn has_shared_drafts(&self) -> bool {
        matches!(
            self.drafts_backend(),
            DraftsBackend::AttachedPostgres { .. } | DraftsBackend::AttachedLake { .. }
        )
    }

    fn drafts_table(&self) -> String {
        match self.drafts_backend() {
            DraftsBackend::Local => "drafts".to_owned(),
            DraftsBackend::AttachedPostgres { alias } | DraftsBackend::AttachedLake { alias } => {
                format!("{alias}.{DRAFTS_PG_TABLE_NAME}")
            }
        }
    }

    /// `Some(tenant)` when rows must be scoped by an explicit `tenant`
    /// column. Mirrors [`Indexer::events_tenant_scope`].
    fn drafts_tenant_scope(&self) -> Option<&str> {
        match self.drafts_backend() {
            DraftsBackend::Local => None,
            DraftsBackend::AttachedPostgres { .. } | DraftsBackend::AttachedLake { .. } => {
                Some(self.tenant())
            }
        }
    }

    /// Hold a proposed write. Returns it as stored, with its id and hash.
    ///
    /// Writes nothing to `pages`: the target is untouched until
    /// [`Indexer::promote_draft`]. The caller is responsible for having
    /// checked that this author may write the target — see the server's
    /// `create_draft`, which runs the same `may_write_instance` a direct
    /// write would, so a draft cannot be used to stage a write the author
    /// could never make.
    ///
    /// # Errors
    /// When the insert fails.
    pub async fn create_draft(&self, draft: NewDraft) -> Result<DraftInfo, IndexerError> {
        let draft_id = Ulid::new().to_string();
        let hash = content_hash(&draft.content);
        let table = self.drafts_table();
        let tenant = self.drafts_tenant_scope().map(str::to_owned);
        let conn = self.conn.lock().await;

        if let Some(t) = &tenant {
            conn.execute(
                &format!(
                    "INSERT INTO {table} \
                     (tenant, draft_id, target_page_id, content, content_sha256, base_sha256, \
                      author, event_id, status, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'open', CURRENT_TIMESTAMP)"
                ),
                duckdb::params![
                    t,
                    &draft_id,
                    &draft.target_page_id,
                    &draft.content,
                    &hash,
                    &draft.base_sha256,
                    &draft.author,
                    &draft.event_id,
                ],
            )?;
        } else {
            conn.execute(
                &format!(
                    "INSERT INTO {table} \
                     (draft_id, target_page_id, content, content_sha256, base_sha256, \
                      author, event_id, status, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, 'open', CURRENT_TIMESTAMP)"
                ),
                duckdb::params![
                    &draft_id,
                    &draft.target_page_id,
                    &draft.content,
                    &hash,
                    &draft.base_sha256,
                    &draft.author,
                    &draft.event_id,
                ],
            )?;
        }
        drop(conn);
        // A draft is a change a subscriber must be able to see (#474).
        //
        // escurel has two notions of "something happened": a bus event, and an
        // INDEX mutation — and a draft is a row, not a page, so it was neither.
        // Consumers therefore never woke for one. That mattered the moment
        // drafts became the main producer: `escurel-runner` drafts every
        // sorted-in capture, and heron's review feed subscribes to exactly
        // these two signals, so the queue grew in silence and the consultant's
        // screen read "nothing waiting" — indistinguishable from a runner that
        // never ran.
        //
        // The epoch is the right lever precisely because a wake is a SIGNAL,
        // not data: every subscriber re-reads its own scoped query and decides
        // for itself whether anything it cares about moved. The cost of a
        // spurious wake is one read; the cost of a missing one is a queue
        // nobody is told about.
        self.bump_mutation_epoch();

        self.get_draft(&draft_id)
            .await?
            // Not a `NotFound` — the row was just inserted, so its absence
            // is a storage fault, not a missing draft. Reported as one.
            .ok_or_else(|| {
                IndexerError::InvalidCursor(format!("draft {draft_id} vanished after insert"))
            })
    }

    /// One draft by id, whatever its status.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn get_draft(&self, draft_id: &str) -> Result<Option<DraftInfo>, IndexerError> {
        let table = self.drafts_table();
        let tenant = self.drafts_tenant_scope().map(str::to_owned);
        let conn = self.conn.lock().await;
        let (sql, params): (String, Vec<String>) = match &tenant {
            Some(t) => (
                format!("{} WHERE tenant = ? AND draft_id = ?", select_cols(&table)),
                vec![t.clone(), draft_id.to_owned()],
            ),
            None => (
                format!("{} WHERE draft_id = ?", select_cols(&table)),
                vec![draft_id.to_owned()],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(duckdb::params_from_iter(params.iter()), row_to_draft)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// The OPEN draft against `target_page_id`, if there is one.
    ///
    /// Exists so a second draft against a page that already has one can be
    /// refused at draft time rather than discovered at review time: promoting
    /// either of two open drafts moves the page, which makes the other's
    /// `base_sha256` stale for ever. See `tool_create_draft`.
    ///
    /// Deliberately unfiltered by reader, like [`Self::list_drafts`]: the
    /// question is "does this page already have one?", which is a fact about
    /// the page and not about who is asking. The caller has already been
    /// admitted to WRITE this page by the time it asks.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn open_draft_for_page(
        &self,
        target_page_id: &str,
    ) -> Result<Option<DraftInfo>, IndexerError> {
        let table = self.drafts_table();
        let tenant = self.drafts_tenant_scope().map(str::to_owned);
        let conn = self.conn.lock().await;
        let (sql, params): (String, Vec<String>) = match &tenant {
            Some(t) => (
                format!(
                    "{} WHERE tenant = ? AND target_page_id = ? AND status = 'open' \
                     ORDER BY created_at DESC LIMIT 1",
                    select_cols(&table)
                ),
                vec![t.clone(), target_page_id.to_owned()],
            ),
            None => (
                format!(
                    "{} WHERE target_page_id = ? AND status = 'open' \
                     ORDER BY created_at DESC LIMIT 1",
                    select_cols(&table)
                ),
                vec![target_page_id.to_owned()],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(duckdb::params_from_iter(params.iter()), row_to_draft)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Every draft still waiting, newest first.
    ///
    /// Deliberately unfiltered by reader: a draft carries no owner column,
    /// and who may see one is decided from the proposed CONTENT's own
    /// frontmatter at the server, exactly as it would be for the page the
    /// draft becomes. Two places that answer "who may read this?" from two
    /// different sources eventually disagree.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn list_drafts(&self, limit: Option<usize>) -> Result<Vec<DraftInfo>, IndexerError> {
        let table = self.drafts_table();
        let tenant = self.drafts_tenant_scope().map(str::to_owned);
        let cap = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let conn = self.conn.lock().await;
        let (sql, params): (String, Vec<String>) = match &tenant {
            Some(t) => (
                format!(
                    "{} WHERE tenant = ? AND status = 'open' ORDER BY created_at DESC{cap}",
                    select_cols(&table)
                ),
                vec![t.clone()],
            ),
            None => (
                format!(
                    "{} WHERE status = 'open' ORDER BY created_at DESC{cap}",
                    select_cols(&table)
                ),
                vec![],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(params.iter()), row_to_draft)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a draft decided. `status` is `promoted` or `discarded`.
    ///
    /// The row is kept, never deleted: "did I already deal with that?" must
    /// stay answerable, which is the same reason the event store has no
    /// delete either.
    ///
    /// Returns `false` when no OPEN draft with that id existed — so a second
    /// promotion of the same draft is a no-op the caller can detect rather
    /// than a second write of the same content.
    ///
    /// # Errors
    /// When the update fails.
    pub async fn close_draft(
        &self,
        draft_id: &str,
        status: &str,
        decided_by: &str,
        reason: &str,
    ) -> Result<bool, IndexerError> {
        let table = self.drafts_table();
        let tenant = self.drafts_tenant_scope().map(str::to_owned);
        let conn = self.conn.lock().await;
        let n = match &tenant {
            Some(t) => conn.execute(
                &format!(
                    "UPDATE {table} SET status = ?, decided_by = ?, reason = ?, \
                     decided_at = CURRENT_TIMESTAMP \
                     WHERE tenant = ? AND draft_id = ? AND status = 'open'"
                ),
                duckdb::params![status, decided_by, reason, t, draft_id],
            )?,
            None => conn.execute(
                &format!(
                    "UPDATE {table} SET status = ?, decided_by = ?, reason = ?, \
                     decided_at = CURRENT_TIMESTAMP \
                     WHERE draft_id = ? AND status = 'open'"
                ),
                duckdb::params![status, decided_by, reason, draft_id],
            )?,
        };
        // Same signal on the way out (#474). A queue that shrinks unannounced
        // is the mirror of one that grows unannounced: two reviewers on two
        // devices, and the card the other one just decided stays on your
        // screen until something unrelated moves the index.
        if n > 0 {
            self.bump_mutation_epoch();
        }
        Ok(n > 0)
    }
}
