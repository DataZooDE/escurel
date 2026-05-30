//! Data layer for the TUI: maps [`DataRequest`]s to gateway RPCs and returns
//! owned [`ScreenData`] DTOs the [`crate::app::App`] can store and render.
//!
//! Keeping all RPC plumbing here (and out of `App`) lets the navigation and
//! render logic be exercised against a [`ratatui::backend::TestBackend`]
//! without a terminal, while this layer is exercised against a real gateway.
//!
//! The DTOs are deliberately owned `String`s (not borrows of the proto
//! responses) so the `App` can hold them across event-loop turns without
//! lifetime gymnastics.

use escurel_client::{
    Client, Edge, ExpandRequest, ListEventsRequest, ListInboxRequest, ListInstancesRequest,
    ListSkillsRequest, NeighboursRequest, SearchRequest, WikilinkParsed,
};

/// A request the event loop should fulfil by talking to the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRequest {
    /// Load the list of skills.
    Skills,
    /// Load the instances of `skill`.
    Instances(String),
    /// Expand a single page (frontmatter + body + outgoing links + backlinks).
    Entity(String),
    /// Load the global inbox.
    Inbox,
    /// Load the processed events for an instance page id.
    Events(String),
    /// Run a full-text search for `query`.
    Search(String),
}

/// One skill row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRow {
    pub id: String,
    pub description: String,
    pub event_typed: bool,
}

/// One instance row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRow {
    pub page_id: String,
    pub skill: String,
    pub at: String,
}

/// An outgoing wikilink from an entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    /// Display target, e.g. `customer::initech`.
    pub target: String,
}

/// A backlink edge pointing at an entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklinkRow {
    pub src_page: String,
    pub link_skill: String,
}

/// A fully expanded entity (frontmatter + body + links + backlinks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityView {
    pub page_id: String,
    pub title: String,
    pub frontmatter_json: String,
    pub body: String,
    pub outgoing_links: Vec<LinkRow>,
    pub backlinks: Vec<BacklinkRow>,
}

/// One inbox / event row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub event_id: String,
    pub title: String,
    pub label_skill: String,
    pub instance_page_id: String,
    pub status: String,
}

/// A search result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    pub page_id: String,
    pub skill: String,
    pub snippet: String,
}

/// The payload loaded for the focused screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenData {
    Skills(Vec<SkillRow>),
    Instances(Vec<InstanceRow>),
    Entity(EntityView),
    Inbox(Vec<EventRow>),
    Events(Vec<EventRow>),
    Search(Vec<SearchRow>),
    Empty,
}

impl ScreenData {
    /// Number of selectable rows (detail views have none).
    pub fn len(&self) -> usize {
        match self {
            ScreenData::Skills(v) => v.len(),
            ScreenData::Instances(v) => v.len(),
            ScreenData::Inbox(v) => v.len(),
            ScreenData::Events(v) => v.len(),
            ScreenData::Search(v) => v.len(),
            ScreenData::Entity(_) | ScreenData::Empty => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Render a parsed wikilink back into its `skill::id` display form. Mirrors the
/// shape the body text used so a "link to initech" assertion can match.
fn wikilink_target(w: &WikilinkParsed) -> String {
    let mut s = String::new();
    if !w.skill.is_empty() {
        s.push_str(&w.skill);
        s.push_str("::");
    }
    s.push_str(&w.id);
    if !w.anchor.is_empty() {
        s.push('#');
        s.push_str(&w.anchor);
    }
    s
}

/// Wraps the typed gateway [`Client`] and turns [`DataRequest`]s into
/// [`ScreenData`]. Entity expansion additionally pulls backlinks via
/// [`Client::neighbours`].
pub struct DataSource {
    client: Client,
}

impl DataSource {
    /// Build a data source over an existing connected client.
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Connect to the gateway and build a data source.
    pub async fn connect(
        endpoint: &str,
        token: escurel_client::SecretString,
    ) -> anyhow::Result<Self> {
        let client = Client::connect(endpoint, token).await?;
        Ok(Self::new(client))
    }

    /// Borrow the underlying client (e.g. to capture events in tests).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Fulfil a [`DataRequest`] by calling the gateway.
    pub async fn fetch(&self, req: &DataRequest) -> anyhow::Result<ScreenData> {
        match req {
            DataRequest::Skills => self.skills().await,
            DataRequest::Instances(skill) => self.instances(skill).await,
            DataRequest::Entity(page_id) => self.entity(page_id).await,
            DataRequest::Inbox => self.inbox().await,
            DataRequest::Events(instance) => self.events(instance).await,
            DataRequest::Search(query) => self.search(query).await,
        }
    }

    async fn skills(&self) -> anyhow::Result<ScreenData> {
        let resp = self
            .client
            .list_skills(ListSkillsRequest::default())
            .await?;
        let rows = resp
            .skills
            .into_iter()
            .map(|s| SkillRow {
                id: s.id,
                description: s.description,
                event_typed: s.is_event_typed,
            })
            .collect();
        Ok(ScreenData::Skills(rows))
    }

    async fn instances(&self, skill: &str) -> anyhow::Result<ScreenData> {
        let resp = self
            .client
            .list_instances(ListInstancesRequest {
                skill: skill.to_string(),
                ..Default::default()
            })
            .await?;
        let rows = resp
            .instances
            .into_iter()
            .map(|i| InstanceRow {
                page_id: i.page_id,
                skill: i.skill,
                at: i.at,
            })
            .collect();
        Ok(ScreenData::Instances(rows))
    }

    async fn entity(&self, page_id: &str) -> anyhow::Result<ScreenData> {
        let resp = self
            .client
            .expand(ExpandRequest {
                page_id: page_id.to_string(),
                ..Default::default()
            })
            .await?;
        // PageRef has no human title field; use the slug as the label and fall
        // back to the page id. The rendered body (`# Acme Corp`) carries the
        // display name itself.
        let (resolved_page_id, title) = match resp.page {
            Some(p) if !p.slug.is_empty() => (p.page_id, p.slug),
            Some(p) => {
                let id = p.page_id.clone();
                (p.page_id, id)
            }
            None => (page_id.to_string(), page_id.to_string()),
        };
        let outgoing_links = resp
            .wikilinks_out
            .iter()
            .map(|w| LinkRow {
                target: wikilink_target(w),
            })
            .collect();
        // Backlinks come from the neighbours RPC (direction "in"); tolerate
        // failure so the entity still renders if the graph lookup errors.
        let backlinks = match self
            .client
            .neighbours(NeighboursRequest {
                page_id: resolved_page_id.clone(),
                direction: "in".to_string(),
                ..Default::default()
            })
            .await
        {
            Ok(n) => n
                .edges
                .into_iter()
                .map(|e: Edge| BacklinkRow {
                    src_page: e.src_page,
                    link_skill: e.link_skill,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        Ok(ScreenData::Entity(EntityView {
            page_id: resolved_page_id,
            title,
            frontmatter_json: resp.frontmatter_json,
            body: resp.body,
            outgoing_links,
            backlinks,
        }))
    }

    async fn inbox(&self) -> anyhow::Result<ScreenData> {
        let resp = self.client.list_inbox(ListInboxRequest::default()).await?;
        Ok(ScreenData::Inbox(
            resp.events.into_iter().map(event_row).collect(),
        ))
    }

    async fn events(&self, instance: &str) -> anyhow::Result<ScreenData> {
        let resp = self
            .client
            .list_events(ListEventsRequest {
                instance_page_id: instance.to_string(),
                ..Default::default()
            })
            .await?;
        Ok(ScreenData::Events(
            resp.events.into_iter().map(event_row).collect(),
        ))
    }

    async fn search(&self, query: &str) -> anyhow::Result<ScreenData> {
        let resp = self
            .client
            .search(SearchRequest {
                q: query.to_string(),
                ..Default::default()
            })
            .await?;
        let rows = resp
            .hits
            .into_iter()
            .map(|h| SearchRow {
                page_id: h.page_id,
                skill: h.skill,
                snippet: h.snippet,
            })
            .collect();
        Ok(ScreenData::Search(rows))
    }
}

fn event_row(e: escurel_client::Event) -> EventRow {
    EventRow {
        event_id: e.event_id,
        title: e.title,
        label_skill: e.label_skill,
        instance_page_id: e.instance_page_id,
        status: e.status,
    }
}
