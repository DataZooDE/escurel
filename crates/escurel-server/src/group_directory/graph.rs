//! Microsoft Graph: `GET /v1.0/users/{objectId}/transitiveMemberOf/microsoft.graph.group`,
//! paged by `@odata.nextLink`, with a client-credentials token (application
//! permission `GroupMember.Read.All`, admin-consented). Keyed by the Entra
//! object id Teams names the sender with.

use std::sync::Arc;

use escurel_auth::ClientCredentials;

use super::{
    DirectoryError, DirectoryGroup, DirectoryUser, GroupDirectory, GroupsFuture, MAX_PAGES,
    NameRule, get_json, is_object_id, service_token,
};
use crate::egress::Egress;

/// Microsoft's public Graph endpoint.
pub const DEFAULT_BASE_URL: &str = "https://graph.microsoft.com";
/// The scope a client-credentials token for Graph asks for.
pub const SCOPE: &str = "https://graph.microsoft.com/.default";

/// The token endpoint of an Entra tenant.
#[must_use]
pub fn default_token_url(tenant_id: &str) -> String {
    format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token")
}

/// `ESCUREL_AUTH_GRAPH_*`.
#[derive(Clone)]
pub struct GraphConfig {
    /// The Entra tenant the bridge's people are in; a turn naming another is refused.
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub token_url: String,
    pub base_url: String,
    /// Keyed by group object id in the map file; the prefix reads `displayName`.
    pub rule: NameRule,
}

impl std::fmt::Debug for GraphConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("token_url", &self.token_url)
            .field("base_url", &self.base_url)
            .field("rule", &self.rule)
            .finish_non_exhaustive()
    }
}

pub struct GraphDirectory {
    base_url: String,
    credentials: ClientCredentials,
    egress: Arc<Egress>,
}

impl GraphDirectory {
    #[must_use]
    pub fn new(cfg: GraphConfig, egress: Arc<Egress>) -> Self {
        Self {
            credentials: ClientCredentials::new(
                cfg.token_url,
                cfg.client_id,
                cfg.client_secret,
                SCOPE,
            ),
            base_url: cfg.base_url,
            egress,
        }
    }

    async fn fetch(&self, object_id: &str) -> Result<Vec<DirectoryGroup>, DirectoryError> {
        let base = reqwest::Url::parse(&self.base_url)
            .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
        let mut url = format!(
            "{}/v1.0/users/{object_id}/transitiveMemberOf/microsoft.graph.group?$select=id,displayName",
            self.base_url.trim_end_matches('/')
        );
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let token = service_token(&self.egress, &self.credentials).await?;
            let got = get_json(
                &self.egress,
                "graph",
                &base,
                &url,
                &format!("Bearer {token}"),
            )
            .await;
            if matches!(&got, Err(DirectoryError::Permanent(why)) if why.ends_with("401")) {
                self.credentials.invalidate().await;
            }
            let (body, _) = got?;
            out.extend(
                body["value"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| {
                        Some(DirectoryGroup {
                            id: g["id"].as_str()?.to_owned(),
                            name: g["displayName"].as_str().unwrap_or_default().to_owned(),
                        })
                    }),
            );
            match body["@odata.nextLink"].as_str() {
                Some(next) => url = next.to_owned(),
                None => return Ok(out),
            }
        }
        Err(DirectoryError::Permanent(format!(
            "more than {MAX_PAGES} pages of groups"
        )))
    }
}

impl GroupDirectory for GraphDirectory {
    fn name(&self) -> &'static str {
        "graph"
    }

    fn lookup_key(&self, user: &DirectoryUser) -> Option<String> {
        user.object_id.clone().filter(|id| is_object_id(id))
    }

    fn groups_for<'a>(&'a self, key: &'a str) -> GroupsFuture<'a> {
        Box::pin(self.fetch(key))
    }
}
