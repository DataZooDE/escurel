//! Microsoft Graph: `GET /v1.0/users/{objectId}/transitiveMemberOf/microsoft.graph.group`,
//! paged by `@odata.nextLink`, with a client-credentials token (application
//! permission `GroupMember.Read.All`, admin-consented). Keyed by the Entra
//! object id Teams names the sender with. The same app reads the sender's
//! `mail` / `userPrincipalName` / `otherMails` (`User.Read.All`) so the
//! bridge's asserted email cannot name someone else (crew S-F2).
//!
//! The prefix rule reads only a SECURITY group's `displayName` by default: a
//! Microsoft 365 group (`groupTypes: [Unified]`, every Team is one) is named
//! by whoever creates it, which under Entra's defaults is any employee
//! (crew S-F1). `ESCUREL_AUTH_GRAPH_INCLUDE_M365_GROUPS=true` lets them count
//! where creating them is restricted; the id-keyed map file counts any group.

use std::sync::Arc;

use escurel_auth::ClientCredentials;

use super::{
    AddressesFuture, DirectoryError, DirectoryGroup, DirectoryUser, GroupDirectory, GroupsFuture,
    MAX_PAGES, NameRule, authorized_get, is_object_id,
};
use crate::egress::Egress;

/// Microsoft's public Graph endpoint.
pub const DEFAULT_BASE_URL: &str = "https://graph.microsoft.com";
/// The scope a client-credentials token for Graph asks for.
pub const SCOPE: &str = "https://graph.microsoft.com/.default";

/// Graph treats an OData cast on a directory-object listing as an ADVANCED
/// query, which needs `ConsistencyLevel: eventual` (with `$count=true` in the
/// URL); every page is asked with it, `@odata.nextLink` pages included
/// (crew U-F3).
const ADVANCED_QUERY: &[(&str, &str)] = &[("ConsistencyLevel", "eventual")];

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
    /// Whether a Microsoft 365 (Unified) group's name counts for the prefix
    /// rule (`ESCUREL_AUTH_GRAPH_INCLUDE_M365_GROUPS`, default no).
    pub include_m365_groups: bool,
}

impl std::fmt::Debug for GraphConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("token_url", &self.token_url)
            .field("base_url", &self.base_url)
            .field("rule", &self.rule)
            .field("include_m365_groups", &self.include_m365_groups)
            .finish_non_exhaustive()
    }
}

pub struct GraphDirectory {
    base_url: String,
    include_m365_groups: bool,
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
            include_m365_groups: cfg.include_m365_groups,
            egress,
        }
    }

    /// `mail`, `userPrincipalName` and `otherMails` of the user (application
    /// permission `User.Read.All`), lowercase; a 404 is
    /// [`DirectoryError::NotFound`].
    async fn addresses(&self, object_id: &str) -> Result<Vec<String>, DirectoryError> {
        if !is_object_id(object_id) {
            return Err(DirectoryError::NotFound("not an object id".to_owned()));
        }
        let base = reqwest::Url::parse(&self.base_url)
            .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
        let url = format!(
            "{}/v1.0/users/{object_id}?$select=mail,userPrincipalName,otherMails",
            self.base_url.trim_end_matches('/')
        );
        let (body, _) =
            authorized_get(&self.egress, &self.credentials, "graph", &base, &url, &[]).await?;
        let mut out: Vec<String> = ["mail", "userPrincipalName"]
            .iter()
            .filter_map(|k| body[*k].as_str())
            .chain(
                body["otherMails"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str),
            )
            .filter(|a| !a.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn fetch(&self, object_id: &str) -> Result<Vec<DirectoryGroup>, DirectoryError> {
        let base = reqwest::Url::parse(&self.base_url)
            .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
        let mut url = format!(
            "{}/v1.0/users/{object_id}/transitiveMemberOf/microsoft.graph.group?$select=id,displayName,securityEnabled,groupTypes&$count=true",
            self.base_url.trim_end_matches('/')
        );
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let (body, _) = authorized_get(
                &self.egress,
                &self.credentials,
                "graph",
                &base,
                &url,
                ADVANCED_QUERY,
            )
            .await?;
            out.extend(
                body["value"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| {
                        Some(DirectoryGroup {
                            id: g["id"].as_str()?.to_owned(),
                            name: g["displayName"].as_str().unwrap_or_default().to_owned(),
                            name_counts: name_counts(g, self.include_m365_groups),
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

/// Whether a group's `displayName` is the prefix rule's to read: a security
/// group's (an admin creates those), and a Microsoft 365 group's only when
/// the deployment opted in. A missing field counts as the unsafe answer.
fn name_counts(g: &serde_json::Value, include_m365_groups: bool) -> bool {
    let unified = g["groupTypes"]
        .as_array()
        .is_none_or(|t| t.iter().any(|t| t == "Unified"));
    if unified {
        include_m365_groups
    } else {
        g["securityEnabled"].as_bool() == Some(true)
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

    fn addresses_of<'a>(&'a self, object_id: &'a str) -> Option<AddressesFuture<'a>> {
        Some(Box::pin(self.addresses(object_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_security_groups_name_counts_unless_m365_groups_are_opted_in() {
        let security = json!({ "securityEnabled": true, "groupTypes": [] });
        let unified = json!({ "securityEnabled": true, "groupTypes": ["Unified"] });
        let distribution = json!({ "securityEnabled": false, "groupTypes": [] });
        let unknown = json!({ "displayName": "escurel-finance" });
        assert!(name_counts(&security, false));
        assert!(
            !name_counts(&unified, false),
            "a Team is named by its creator"
        );
        assert!(name_counts(&unified, true));
        assert!(!name_counts(&distribution, false));
        assert!(!name_counts(&distribution, true));
        assert!(!name_counts(&unknown, false));
    }

    /// Crew Q-F3 + U-F3: a 401 renews the service token and asks once more;
    /// every page carries the advanced-query header and `$count=true`.
    #[tokio::test]
    async fn a_rejected_token_is_renewed_once_and_every_page_is_an_advanced_query() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};
        let idp = MockServer::start().await;
        let issued = Arc::new(AtomicUsize::new(0));
        let n = Arc::clone(&issued);
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(move |_: &Request| {
                let i = n.fetch_add(1, Ordering::SeqCst) + 1;
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "access_token": format!("t-{i}"), "expires_in": 3600 }))
            })
            .mount(&idp)
            .await;
        let listing = "/v1.0/users/oid/transitiveMemberOf/microsoft.graph.group";
        let base = idp.uri();
        Mock::given(method("GET"))
            .and(path(listing))
            .and(header("ConsistencyLevel", "eventual"))
            .and(query_param("$count", "true"))
            .respond_with(move |req: &Request| {
                let auth = req
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok());
                if auth != Some("Bearer t-2") {
                    return ResponseTemplate::new(401);
                }
                let page2 = req.url.query_pairs().any(|(k, _)| k == "$skiptoken");
                let mut body = json!({ "value": [{ "id": if page2 { "g2" } else { "g1" },
                    "displayName": "escurel-x", "securityEnabled": true, "groupTypes": [] }] });
                if !page2 {
                    body["@odata.nextLink"] = json!(format!(
                        "{base}{listing}?$select=id&$count=true&$skiptoken=2"
                    ));
                }
                ResponseTemplate::new(200).set_body_json(body)
            })
            .mount(&idp)
            .await;
        let egress = Arc::new(Egress::new(crate::egress::EgressPolicy {
            allow_loopback: true,
            ..Default::default()
        }));
        let dir = GraphDirectory::new(
            GraphConfig {
                tenant_id: "t".into(),
                client_id: "c".into(),
                client_secret: "s".into(),
                token_url: format!("{}/token", idp.uri()),
                base_url: idp.uri(),
                rule: NameRule::default(),
                include_m365_groups: false,
            },
            egress,
        );
        let got = dir.groups_for("oid").await.expect("renewed and answered");
        let ids: Vec<&str> = got.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["g1", "g2"]);
        assert_eq!(
            issued.load(Ordering::SeqCst),
            2,
            "one renewal, then the cached token"
        );
    }
}
