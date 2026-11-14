//! Okta: `GET {base}/api/v1/users/{login}/groups`, paged by the `Link: …;
//! rel="next"` header, as either
//!
//! * an API token (`ESCUREL_AUTH_OKTA_AUTH=ssws`, `Authorization: SSWS
//!   <token>`, a read-only admin's), or
//! * an OAuth 2.0 service app (`ESCUREL_AUTH_OKTA_AUTH=oauth`): a
//!   client-credentials grant at the org authorization server for
//!   `okta.groups.read okta.users.read`, the client authenticated by an
//!   assertion signed with the app's private key (`private_key_jwt`), then
//!   `Authorization: Bearer <token>`. Okta's "Require DPoP" must be off on
//!   the app: escurel sends no DPoP proof, and says so when Okta asks for one.
//!
//! Keyed by email, or by UPN (`ESCUREL_AUTH_OKTA_USER_KEY=upn`) where Okta
//! logins are UPNs.
//!
//! Only an `OKTA_GROUP`'s name (one an Okta admin creates) counts for the
//! prefix rule by default; an `APP_GROUP` (named by an app's import) counts
//! with `ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS=true`, a `BUILT_IN` one never
//! (crew S-F1). The id-keyed map file counts any group.

use std::sync::Arc;

use escurel_auth::ClientCredentials;

use super::{
    DirectoryError, DirectoryGroup, DirectoryUser, GroupDirectory, GroupsFuture, MAX_PAGES,
    NameRule, authorized_get, get_json,
};
use crate::egress::Egress;

/// The scopes a service app's token asks for; the app must be granted both.
pub const SCOPE: &str = "okta.groups.read okta.users.read";

/// The org authorization server's token endpoint: the management API's
/// `okta.*` scopes are granted only there, never by a custom server.
#[must_use]
pub fn default_token_url(base_url: &str) -> String {
    format!("{}/oauth2/v1/token", base_url.trim_end_matches('/'))
}

/// How the gateway authenticates to Okta (`ESCUREL_AUTH_OKTA_AUTH`).
#[derive(Clone)]
pub enum OktaAuth {
    /// `ssws`: a read-only admin's API token.
    ApiToken(String),
    /// `oauth`: an API Services app with a public-key client.
    ServiceApp {
        client_id: String,
        /// The app's RSA private key (PEM).
        private_key: String,
        /// The key's `kid` in the app's JWKS, when the app holds several.
        key_id: Option<String>,
        token_url: String,
    },
}

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
    pub auth: OktaAuth,
    pub user_key: UserKey,
    /// Keyed by group id in the map file; the prefix reads `profile.name`.
    pub rule: NameRule,
    /// Whether an `APP_GROUP`'s name counts (`ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS`).
    pub include_app_groups: bool,
}

impl std::fmt::Debug for OktaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("OktaConfig");
        d.field("base_url", &self.base_url);
        match &self.auth {
            OktaAuth::ApiToken(_) => d.field("auth", &"ssws"),
            OktaAuth::ServiceApp {
                client_id,
                key_id,
                token_url,
                ..
            } => d
                .field("auth", &"oauth")
                .field("client_id", client_id)
                .field("key_id", key_id)
                .field("token_url", token_url),
        };
        d.field("user_key", &self.user_key)
            .field("rule", &self.rule)
            .field("include_app_groups", &self.include_app_groups)
            .finish_non_exhaustive()
    }
}

impl OktaConfig {
    /// Refuse, at boot, a service app's key that cannot sign — not at the
    /// first turn.
    ///
    /// # Errors
    /// The key is not an RSA private key, as an operator-facing sentence.
    pub fn check(&self) -> Result<(), String> {
        self.credential().map(drop)
    }

    /// One construction for the boot check and the directory.
    fn credential(&self) -> Result<Credential, String> {
        match &self.auth {
            OktaAuth::ApiToken(token) => Ok(Credential::Ssws(format!("SSWS {token}"))),
            OktaAuth::ServiceApp {
                client_id,
                private_key,
                key_id,
                token_url,
            } => ClientCredentials::private_key_jwt(
                token_url.clone(),
                client_id.clone(),
                key_id.clone(),
                private_key,
                SCOPE,
            )
            .map(|cc| Credential::ServiceApp(Box::new(cc)))
            .map_err(|e| format!("ESCUREL_AUTH_OKTA_PRIVATE_KEY: {e}")),
        }
    }
}

/// The credential every listing page carries.
enum Credential {
    /// The whole `Authorization` value, `SSWS <token>`.
    Ssws(String),
    ServiceApp(Box<ClientCredentials>),
}

pub struct OktaDirectory {
    cfg: OktaConfig,
    credential: Credential,
    egress: Arc<Egress>,
}

impl OktaDirectory {
    /// # Errors
    /// A service app's private key is not an RSA private key.
    pub fn new(cfg: OktaConfig, egress: Arc<Egress>) -> Result<Self, String> {
        let credential = cfg.credential()?;
        Ok(Self {
            cfg,
            credential,
            egress,
        })
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
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let (body, headers) = match &self.credential {
                Credential::Ssws(auth) => {
                    get_json(&self.egress, "okta", &base, &url, auth, &[]).await?
                }
                Credential::ServiceApp(cc) => {
                    authorized_get(&self.egress, cc, "okta", &base, &url, &[]).await?
                }
            };
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
        match self.credential {
            Credential::Ssws(_) => {
                "ESCUREL_AUTH_OKTA_API_TOKEN must belong to an admin who can read users and groups (Read-only Administrator), and the sender's email (or UPN, ESCUREL_AUTH_OKTA_USER_KEY) must be their Okta login"
            }
            Credential::ServiceApp(_) => {
                "the Okta API Services app needs a public-key client whose key is ESCUREL_AUTH_OKTA_PRIVATE_KEY, the granted scopes okta.groups.read and okta.users.read, the Read-only Administrator role, and \"Require DPoP\" off; the sender's email (or UPN, ESCUREL_AUTH_OKTA_USER_KEY) must be their Okta login"
            }
        }
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
