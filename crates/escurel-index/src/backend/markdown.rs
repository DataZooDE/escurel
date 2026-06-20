//! The default backend: native markdown pages, delegating to [`Indexer`].
//!
//! Every method is a one-line delegate to the corresponding `Indexer`
//! method — no logic lives here. This is what makes PR-1 a pure refactor:
//! the markdown path is unchanged, merely reachable through the trait.

use std::sync::Arc;

use async_trait::async_trait;
use escurel_md::PageType;

use super::{BackendCtx, BackendKind, Capabilities, InstanceBackend, SearchMode};
use crate::read::{Direction, Edge, ExpandedPage, InstanceInfo, OrderDir, ResolvedWikilink};
use crate::search::{Granularity, SearchHit};
use crate::validate::Issue;
use crate::{Indexer, IndexerError};

/// Markdown-backed instances: the today behaviour, wrapped behind the
/// [`InstanceBackend`] trait. Holds an `Arc<Indexer>` and delegates.
pub struct MarkdownBackend {
    indexer: Arc<Indexer>,
}

impl MarkdownBackend {
    /// Wrap a shared indexer as the markdown backend.
    #[must_use]
    pub fn new(indexer: Arc<Indexer>) -> Self {
        Self { indexer }
    }
}

#[async_trait]
impl InstanceBackend for MarkdownBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Markdown
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            granularity: Granularity::Block,
            search: SearchMode::Hybrid,
            supports_crdt: true,
        }
    }

    async fn list(
        &self,
        ctx: BackendCtx<'_>,
        skill: &str,
        order_by_at: Option<OrderDir>,
        limit: Option<usize>,
        filter: Option<(&str, &str)>,
    ) -> Result<Vec<InstanceInfo>, IndexerError> {
        self.indexer
            .list_instances(skill, order_by_at, limit, filter, ctx.as_of, ctx.scenario)
            .await
    }

    async fn resolve(
        &self,
        ctx: BackendCtx<'_>,
        wikilink: &str,
    ) -> Result<ResolvedWikilink, IndexerError> {
        self.indexer.resolve(wikilink, ctx.scenario).await
    }

    async fn expand(
        &self,
        ctx: BackendCtx<'_>,
        page_id: &str,
    ) -> Result<Option<ExpandedPage>, IndexerError> {
        self.indexer.expand(page_id, ctx.as_of, ctx.scenario).await
    }

    async fn neighbours(
        &self,
        ctx: BackendCtx<'_>,
        page_id: &str,
        direction: Direction,
        link_skill_filter: Option<&str>,
    ) -> Result<Vec<Edge>, IndexerError> {
        self.indexer
            .neighbours(
                page_id,
                direction,
                link_skill_filter,
                ctx.as_of,
                ctx.scenario,
            )
            .await
    }

    async fn search_contribution(
        &self,
        ctx: BackendCtx<'_>,
        q: &str,
        k: usize,
        page_type: Option<PageType>,
        skill: Option<&str>,
        granularity: Granularity,
        filter: Option<&serde_json::Value>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        self.indexer
            .search_with(
                q,
                k,
                page_type,
                skill,
                ctx.as_of,
                ctx.scenario,
                granularity,
                filter,
            )
            .await
    }

    async fn validate(
        &self,
        _ctx: BackendCtx<'_>,
        page_id: Option<&str>,
        content: &str,
    ) -> Result<Vec<Issue>, IndexerError> {
        self.indexer.validate(page_id, content).await
    }
}
