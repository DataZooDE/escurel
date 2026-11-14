//! Okta: `GET {base}/api/v1/users/{login}/groups`, paged by the `Link: …;
//! rel="next"` header, with an API token (`Authorization: SSWS <token>`, a
//! read-only admin's). Keyed by email, or by UPN
//! (`ESCUREL_AUTH_OKTA_USER_KEY=upn`) where Okta logins are UPNs.
//!
//! Only an `OKTA_GROUP`'s name (one an Okta admin creates) counts for the
//! prefix rule by default; an `APP_GROUP` (named by an app's import) counts
//! with `ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS=true`, a `BUILT_IN` one never
//! (crew S-F1). The id-keyed map file counts any group.
//!
//! OAuth for Okta service apps (client credentials with a private key) is not
//! implemented; `docs/deploy/chat-bridge.md` says so.

use std::sync::Arc;

use super::{
    DirectoryError, DirectoryGroup, DirectoryUser, GroupDirectory, GroupsFuture, MAX_PAGES,
    NameRule, get_json,
};
use crate::egress::Egress;

/// Which of the sender's fields is their Okta login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UserKey {
    #[default]
    Email,
    Upn,
}

/// `ESCUREL_AUTH_OKTA_*`.
#[derive(Clone)]
pub struct OktaConfig {
    /// `https://<org>.okta.com`.
    pub base_url: String,
    pub api_token: String,
    pub user_key: UserKey,
    /// Keyed by group id in the map file; the prefix reads `profile.name`.
    pub rule: NameRule,
    /// Whether an `APP_GROUP`'s name counts (`ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS`).
    pub include_app_groups: bool,
}

impl std::fmt::Debug for OktaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OktaConfig")
            .field("base_url", &self.base_url)
            .field("user_key", &self.user_key)
            .field("rule", &self.rule)
            .field("include_app_groups", &self.include_app_groups)
            .finish_non_exhaustive()
    }
}

pub struct OktaDirectory {
    cfg: OktaConfig,
    egress: Arc<Egress>,
}

impl OktaDirectory {
    #[must_use]
    pub fn new(cfg: OktaConfig, egress: Arc<Egress>) -> Self {
        Self { cfg, egress }
    }

    async fn fetch(&self, login: &str) -> Result<Vec<DirectoryGroup>, DirectoryError> {
        let base = reqwest::Url::parse(&self.cfg.base_url)
            .map_err(|e| DirectoryError::Permanent(e.to_string()))?;
        let mut first = base.clone();
        first
            .path_segments_mut()
            .map_err(|()| DirectoryError::Permanent("not a base URL".to_owned()))?
            .pop_if_empty()
            .extend(["api", "v1", "users", login, "groups"]);
        first.query_pairs_mut().append_pair("limit", "200");
        let mut url = first.to_string();
        let auth = format!("SSWS {}", self.cfg.api_token);
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let (body, headers) = get_json(&self.egress, "okta", &base, &url, &auth, &[]).await?;
            out.extend(body.as_array().into_iter().flatten().filter_map(|g| {
                Some(DirectoryGroup {
                    id: g["id"].as_str()?.to_owned(),
                    name: g["profile"]["name"].as_str().unwrap_or_default().to_owned(),
                    name_counts: match g["type"].as_str() {
                        Some("OKTA_GROUP") => true,
                        Some("APP_GROUP") => self.cfg.include_app_groups,
                        _ => false,
                    },
                })
            }));
            match next_link(&headers) {
                Some(next) => url = next,
                None => return Ok(out),
            }
        }
        Err(DirectoryError::Permanent(format!(
            "more than {MAX_PAGES} pages of groups"
        )))
    }
}

/// The `rel="next"` target of the `Link` headers, if any.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get_all(reqwest::header::LINK)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|link| {
            let (target, params) = link.split_once(';')?;
            params
                .split(';')
                .any(|p| p.trim().replace(' ', "") == "rel=\"next\"")
                .then(|| {
                    target
                        .trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned()
                })
        })
}

impl GroupDirectory for OktaDirectory {
    fn name(&self) -> &'static str {
        "okta"
    }

    fn setup_hint(&self) -> &'static str {
        "ESCUREL_AUTH_OKTA_API_TOKEN must belong to an admin who can read users and groups (Read-only Administrator), and the sender's email (or UPN, ESCUREL_AUTH_OKTA_USER_KEY) must be their Okta login"
    }

    fn lookup_key(&self, user: &DirectoryUser) -> Option<String> {
        // Okta logins are case-insensitive; one person is one cache entry.
        match self.cfg.user_key {
            UserKey::Email => user.email.as_deref(),
            UserKey::Upn => user.upn.as_deref(),
        }
        .filter(|k| !k.is_empty() && k.len() <= 254)
        .map(str::to_ascii_lowercase)
    }

    fn groups_for<'a>(&'a self, key: &'a str) -> GroupsFuture<'a> {
        Box::pin(self.fetch(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_link_is_read_from_the_link_headers() {
        let mut h = reqwest::header::HeaderMap::new();
        h.append(
            reqwest::header::LINK,
            "<https://o.okta.com/api/v1/users/a/groups?limit=200>; rel=\"self\""
                .parse()
                .unwrap(),
        );
        h.append(
            reqwest::header::LINK,
            "<https://o.okta.com/api/v1/users/a/groups?after=g2&limit=200>; rel=\"next\""
                .parse()
                .unwrap(),
        );
        assert_eq!(
            next_link(&h).as_deref(),
            Some("https://o.okta.com/api/v1/users/a/groups?after=g2&limit=200")
        );
        h.remove(reqwest::header::LINK);
        assert_eq!(next_link(&h), None);
    }
}
