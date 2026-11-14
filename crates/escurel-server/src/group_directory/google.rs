//! Google Cloud Identity: `GET /v1/groups/-/memberships:searchTransitiveGroups`
//! for `member_key_id == '<email>'`, paged by `nextPageToken`, with a token
//! from a service account's JWT-bearer grant (scope
//! `cloud-identity.groups.readonly`; domain-wide delegation via `subject`, or
//! the service account holding the Groups Reader role). Keyed by email.
//!
//! A group is named by its whole address by default: anyone who may create a
//! group in ANY domain could otherwise call one `escurel-finance@…` (crew
//! S-F1). `ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN` pins the domains whose groups'
//! names count; `email_local` naming needs it.

use std::sync::Arc;

use escurel_auth::ClientCredentials;

use super::{
    DirectoryError, DirectoryGroup, DirectoryUser, GroupDirectory, GroupsFuture, MAX_PAGES,
    NameRule, get_json, service_token,
};
use crate::egress::Egress;

/// Google's public Cloud Identity endpoint.
pub const DEFAULT_BASE_URL: &str = "https://cloudidentity.googleapis.com";
/// Google's OAuth token endpoint (a key file's `token_uri` names it too).
pub const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-identity.groups.readonly";

/// What a Google group is called before the name rule reads it
/// (`ESCUREL_AUTH_GOOGLE_GROUP_NAME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupName {
    /// The whole group email (the default).
    #[default]
    Email,
    /// `sales@acme.example` → `sales`; only with pinned domains.
    EmailLocal,
}

/// `ESCUREL_AUTH_GOOGLE_*`, with the service-account key already read.
#[derive(Clone)]
pub struct GoogleConfig {
    /// The key file's `client_email`: the assertion's issuer.
    pub client_email: String,
    /// The key file's `private_key` (PEM).
    pub private_key: String,
    pub private_key_id: Option<String>,
    /// A Workspace user to act as (domain-wide delegation), when set.
    pub subject: Option<String>,
    pub token_url: String,
    pub base_url: String,
    pub group_name: GroupName,
    /// The domains whose groups' names the prefix rule reads
    /// (`ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN`, lowercase); empty = any domain.
    pub group_domains: Vec<String>,
    /// Keyed by group email (`groupKey.id`) in the map file.
    pub rule: NameRule,
}

impl std::fmt::Debug for GoogleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleConfig")
            .field("client_email", &self.client_email)
            .field("subject", &self.subject)
            .field("token_url", &self.token_url)
            .field("base_url", &self.base_url)
            .field("group_name", &self.group_name)
            .field("group_domains", &self.group_domains)
            .field("rule", &self.rule)
            .finish_non_exhaustive()
    }
}

pub struct GoogleDirectory {
    base_url: String,
    group_name: GroupName,
    group_domains: Vec<String>,
    credentials: ClientCredentials,
    egress: Arc<Egress>,
}

impl GoogleDirectory {
    /// # Errors
    /// The service-account key is not an RSA private key.
    pub fn new(cfg: GoogleConfig, egress: Arc<Egress>) -> Result<Self, String> {
        let credentials = ClientCredentials::jwt_bearer(
            cfg.token_url,
            cfg.client_email,
            cfg.subject,
            cfg.private_key_id,
            &cfg.private_key,
            SCOPE,
        )
        .map_err(|e| format!("ESCUREL_AUTH_GOOGLE_SA_KEY_FILE: {e}"))?;
        Ok(Self {
            base_url: cfg.base_url,
            group_name: cfg.group_name,
            group_domains: cfg.group_domains,
            credentials,
            egress,
        })
    }

    async fn fetch(&self, email: &str) -> Result<Vec<DirectoryGroup>, DirectoryError> {
        let base = reqwest::Url::parse(&self.base_url)
            .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
        let mut page_token: Option<String> = None;
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let mut url = base
                .join("/v1/groups/-/memberships:searchTransitiveGroups")
                .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
            url.query_pairs_mut().append_pair(
                "query",
                &format!(
                    "member_key_id=='{email}' && \
                     'cloudidentity.googleapis.com/groups.discussion_forum' in labels"
                ),
            );
            if let Some(t) = &page_token {
                url.query_pairs_mut().append_pair("pageToken", t);
            }
            let token = service_token(&self.egress, &self.credentials).await?;
            let (body, _) = get_json(
                &self.egress,
                "google",
                &base,
                url.as_str(),
                &format!("Bearer {token}"),
            )
            .await?;
            for m in body["memberships"].as_array().into_iter().flatten() {
                let Some(id) = m["groupKey"]["id"].as_str() else {
                    continue;
                };
                let (local, domain) = id.split_once('@').unwrap_or((id, ""));
                let name = match self.group_name {
                    GroupName::Email => id,
                    GroupName::EmailLocal => local,
                };
                out.push(DirectoryGroup {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    name_counts: self.group_domains.is_empty()
                        || self
                            .group_domains
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(domain)),
                });
            }
            page_token = body["nextPageToken"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            if page_token.is_none() {
                return Ok(out);
            }
        }
        Err(DirectoryError::Permanent(format!(
            "more than {MAX_PAGES} pages of groups"
        )))
    }
}

/// An address usable inside the search query's quotes.
pub(crate) fn is_plain_email(s: &str) -> bool {
    s.len() <= 254
        && s.split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
        && !s
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
}

impl GroupDirectory for GoogleDirectory {
    fn name(&self) -> &'static str {
        "google"
    }

    fn lookup_key(&self, user: &DirectoryUser) -> Option<String> {
        user.email.clone().filter(|e| is_plain_email(e))
    }

    fn groups_for<'a>(&'a self, key: &'a str) -> GroupsFuture<'a> {
        Box::pin(self.fetch(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_address_enters_the_query() {
        assert!(is_plain_email("alice@acme.example"));
        for bad in [
            "alice",
            "@acme.example",
            "a'b@acme.example",
            "a b@x.y",
            "a@local",
        ] {
            assert!(!is_plain_email(bad), "{bad}");
        }
    }
}
