//! The asker's escurel groups, from the organisation's group directories (#830).
//!
//! A chat bridge relays a person's turn; the GATEWAY, never the bridge, decides
//! which groups that person holds. It asks every configured directory
//! (`ESCUREL_AUTH_GROUP_DIRECTORIES=graph,google,okta`, any subset) and takes
//! the UNION:
//!
//! * [`graph`] — Microsoft Graph `transitiveMemberOf`, by the Entra object id;
//! * [`google`] — Cloud Identity `searchTransitiveGroups`, by email;
//! * [`okta`] — the Okta users API, by email (or UPN).
//!
//! Each provider only fetches; what is shared lives here: the
//! [`NameRule`] that turns a directory group into an escurel group (a prefix
//! and/or an explicit map, per provider — a reserved or privileged name is
//! never produced), the cache (an answer is reused for
//! [`BridgeMintConfig::cache_ttl`], 10 min; while a provider is unreachable a
//! cached answer stands in for up to [`BridgeMintConfig::stale_ttl`], 1 h, and
//! after that the turn is refused as retryable), and [`get_json`], the one
//! HTTP path every provider takes through the egress policy (https only,
//! checked and pinned addresses, no redirects, a capped body, page links only
//! on the configured host).
//!
//! A provider that cannot key the user (an email-keyed one, for a sender with
//! no email) contributes nothing; a PERMANENT refusal (401/403/404: a
//! misconfiguration or an unknown user) contributes nothing too, logged — it
//! narrows the person's access, never widens it. Only a RETRYABLE failure
//! (429, 5xx, transport) with no young-enough cached answer refuses the turn.

pub mod google;
pub mod graph;
pub mod okta;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::egress::{Egress, EgressError};

/// How long an answer is used without asking again.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a cached answer stands in for a directory that cannot be reached.
pub const DEFAULT_STALE_TTL: Duration = Duration::from_secs(60 * 60);
/// A user in more groups than this many pages is refused rather than half-read.
pub(crate) const MAX_PAGES: usize = 50;

/// What the bridge asserts about the asker; each provider keys on its own field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryUser {
    /// The Entra object id (Teams' `aadObjectId`).
    pub object_id: Option<String>,
    /// The email address, from the Bot Framework conversation-members API.
    pub email: Option<String>,
    /// The user principal name, from the same.
    pub upn: Option<String>,
}

/// One group as a directory names it: a stable id (what a map file keys on)
/// and a name (what the prefix rule reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryGroup {
    pub id: String,
    pub name: String,
}

/// Why a directory could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    /// Try again later: a 429, a 5xx, a timeout, a dropped connection.
    #[error("{0} (retryable)")]
    Retryable(String),
    /// Asking again will not help: 401/403/404, a refused URL, a bad answer.
    #[error("{0}")]
    Permanent(String),
}

/// A provider's answer, boxed so providers sit behind one trait object.
pub type GroupsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<DirectoryGroup>, DirectoryError>> + Send + 'a>>;

/// One group directory.
pub trait GroupDirectory: Send + Sync {
    /// `graph`, `google`, `okta`: in logs and cache keys.
    fn name(&self) -> &'static str;
    /// What this directory looks the user up by; `None` when the user has no
    /// such field (the provider then contributes nothing).
    fn lookup_key(&self, user: &DirectoryUser) -> Option<String>;
    /// Every group `key` is (transitively) a member of.
    fn groups_for<'a>(&'a self, key: &'a str) -> GroupsFuture<'a>;
}

/// Directory group → escurel group: the explicit map by id first, else the
/// prefix rule by name (only names with the prefix count, prefix stripped).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameRule {
    pub prefix: Option<String>,
    pub map: HashMap<String, String>,
}

impl NameRule {
    /// Whether the rule can yield anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prefix.is_none() && self.map.is_empty()
    }

    /// The escurel group `g` grants, never a reserved or privileged one.
    #[must_use]
    pub fn apply(&self, g: &DirectoryGroup, reserved: &[String]) -> Option<String> {
        let name = match self.map.get(&g.id) {
            Some(mapped) => mapped.clone(),
            None => g.name.strip_prefix(self.prefix.as_deref()?)?.to_owned(),
        };
        (escurel_types::chat_channel::is_assignable_group(&name) && !reserved.contains(&name))
            .then_some(name)
    }
}

/// One configured provider.
#[derive(Debug, Clone)]
pub enum DirectoryConfig {
    Graph(graph::GraphConfig),
    Google(google::GoogleConfig),
    Okta(okta::OktaConfig),
}

impl DirectoryConfig {
    fn rule(&self) -> &NameRule {
        match self {
            Self::Graph(c) => &c.rule,
            Self::Google(c) => &c.rule,
            Self::Okta(c) => &c.rule,
        }
    }
}

/// What `mint_user_token` needs beyond the signing identity: who a sender is
/// (the principal template) and which groups they hold (the directories).
#[derive(Debug, Clone)]
pub struct BridgeMintConfig {
    /// `ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE`, validated by
    /// [`escurel_types::bridge::validate_principal_template`].
    pub principal_template: String,
    /// `ESCUREL_AUTH_GROUP_DIRECTORIES`, in order; at least one.
    pub directories: Vec<DirectoryConfig>,
    pub cache_ttl: Duration,
    pub stale_ttl: Duration,
}

impl BridgeMintConfig {
    /// The Entra tenant the Graph provider looks people up in, if one is configured.
    #[must_use]
    pub fn graph_tenant(&self) -> Option<&str> {
        self.directories.iter().find_map(|d| match d {
            DirectoryConfig::Graph(g) => Some(g.tenant_id.as_str()),
            _ => None,
        })
    }
}

/// The built `mint_user_token` dependencies: the template and the directories.
pub struct BridgeMint {
    pub principal_template: String,
    /// The Entra tenant the Graph provider serves; `None` without Graph.
    pub graph_tenant: Option<String>,
    pub directories: GroupDirectories,
}

impl BridgeMint {
    /// # Errors
    /// See [`GroupDirectories::new`].
    pub fn new(
        cfg: &BridgeMintConfig,
        egress: &Arc<Egress>,
        reserved: Vec<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            principal_template: cfg.principal_template.clone(),
            graph_tenant: cfg.graph_tenant().map(str::to_owned),
            directories: GroupDirectories::new(cfg, egress, reserved)?,
        })
    }
}

/// `(provider name, lookup key)`.
type CacheKey = (&'static str, String);

struct Provider {
    directory: Box<dyn GroupDirectory>,
    rule: NameRule,
}

/// The configured directories, the shared mapping step and the shared cache.
pub struct GroupDirectories {
    providers: Vec<Provider>,
    /// Role values a group may never be called (the deployment's admin,
    /// operator, runner and bridge values).
    reserved: Vec<String>,
    cache_ttl: Duration,
    stale_ttl: Duration,
    /// `(provider, key)` → the mapped groups and when they were fetched.
    cache: Mutex<HashMap<CacheKey, (Vec<String>, Instant)>>,
}

impl GroupDirectories {
    /// # Errors
    /// A provider's credentials cannot be built (a service-account key that
    /// does not parse).
    pub fn new(
        cfg: &BridgeMintConfig,
        egress: &Arc<Egress>,
        reserved: Vec<String>,
    ) -> Result<Self, String> {
        let mut providers = Vec::with_capacity(cfg.directories.len());
        for d in &cfg.directories {
            let directory: Box<dyn GroupDirectory> = match d {
                DirectoryConfig::Graph(c) => {
                    Box::new(graph::GraphDirectory::new(c.clone(), Arc::clone(egress)))
                }
                DirectoryConfig::Google(c) => {
                    Box::new(google::GoogleDirectory::new(c.clone(), Arc::clone(egress))?)
                }
                DirectoryConfig::Okta(c) => {
                    Box::new(okta::OktaDirectory::new(c.clone(), Arc::clone(egress)))
                }
            };
            providers.push(Provider {
                directory,
                rule: d.rule().clone(),
            });
        }
        Ok(Self {
            providers,
            reserved,
            cache_ttl: cfg.cache_ttl,
            stale_ttl: cfg.stale_ttl,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The union of the escurel groups every directory grants `user`, sorted.
    ///
    /// # Errors
    /// [`DirectoryError::Retryable`] when a directory cannot answer and its
    /// cached answer (if any) is older than the stale window.
    pub async fn groups_from_directory(
        &self,
        user: &DirectoryUser,
    ) -> Result<Vec<String>, DirectoryError> {
        let mut union: Vec<String> = Vec::new();
        for p in &self.providers {
            for g in self.provider_groups(p, user).await? {
                if !union.contains(&g) {
                    union.push(g);
                }
            }
        }
        union.sort();
        Ok(union)
    }

    async fn provider_groups(
        &self,
        p: &Provider,
        user: &DirectoryUser,
    ) -> Result<Vec<String>, DirectoryError> {
        let name = p.directory.name();
        let Some(key) = p.directory.lookup_key(user) else {
            tracing::info!(
                directory = name,
                "group directory: the sender has no key for it"
            );
            return Ok(Vec::new());
        };
        let slot = (name, key);
        let cached = self.cache.lock().await.get(&slot).cloned();
        if let Some((groups, at)) = &cached
            && at.elapsed() < self.cache_ttl
        {
            return Ok(groups.clone());
        }
        match p.directory.groups_for(&slot.1).await {
            Ok(found) => {
                let mut groups: Vec<String> = Vec::new();
                for g in &found {
                    if let Some(n) = p.rule.apply(g, &self.reserved)
                        && !groups.contains(&n)
                    {
                        groups.push(n);
                    }
                }
                self.cache
                    .lock()
                    .await
                    .insert(slot, (groups.clone(), Instant::now()));
                Ok(groups)
            }
            Err(DirectoryError::Permanent(why)) => {
                tracing::warn!(directory = name, error = %why,
                    "group directory refused the lookup; it grants nothing for this sender");
                Ok(Vec::new())
            }
            Err(DirectoryError::Retryable(why)) => match cached {
                Some((groups, at)) if at.elapsed() < self.stale_ttl => {
                    tracing::warn!(directory = name, error = %why, age_s = at.elapsed().as_secs(),
                        "group directory unreachable; using the cached answer");
                    Ok(groups)
                }
                _ => {
                    tracing::warn!(directory = name, error = %why, "group directory unreachable");
                    Err(DirectoryError::Retryable(format!("{name}: {why}")))
                }
            },
        }
    }
}

/// An egress refusal as a directory error: a limit or a dropped connection
/// may pass, a refused URL will not.
fn egress_error(e: &EgressError) -> DirectoryError {
    match e {
        EgressError::RateLimited
        | EgressError::Timeout(_)
        | EgressError::Transport(_)
        | EgressError::Unresolvable => DirectoryError::Retryable(e.to_string()),
        _ => DirectoryError::Permanent(e.to_string()),
    }
}

/// A token-endpoint failure as a directory error: 429/5xx and transport pass,
/// everything else (a revoked secret, a bad key) does not.
fn token_error(e: &escurel_auth::ClientCredentialsError) -> DirectoryError {
    use escurel_auth::ClientCredentialsError as E;
    match e {
        E::Transport(_) => DirectoryError::Retryable(e.to_string()),
        E::Status(s) if *s == 429 || *s >= 500 => DirectoryError::Retryable(e.to_string()),
        _ => DirectoryError::Permanent(e.to_string()),
    }
}

/// A service token for `credentials`, fetched through the egress policy.
async fn service_token(
    egress: &Egress,
    credentials: &escurel_auth::ClientCredentials,
) -> Result<String, DirectoryError> {
    let (client, _) = egress
        .client_for(credentials.token_url())
        .await
        .map_err(|e| egress_error(&e))?;
    credentials
        .token(&client)
        .await
        .map_err(|e| token_error(&e))
}

/// One page of a directory's JSON listing: `url` must be on `base`'s scheme,
/// host and port (a page link is the directory's to give, but only on its own
/// host), the call goes through the egress policy under `limiter`, and the
/// status is classified (429/5xx retryable, any other failure permanent).
/// Returns the body and the response headers (Okta pages by `Link`).
///
/// # Errors
/// See above; the message carries no URL query, header or secret.
pub(crate) async fn get_json(
    egress: &Egress,
    limiter: &str,
    base: &reqwest::Url,
    url: &str,
    authorization: &str,
) -> Result<(Value, reqwest::header::HeaderMap), DirectoryError> {
    let (client, parsed) = egress.client_for(url).await.map_err(|e| egress_error(&e))?;
    if parsed.scheme() != base.scheme()
        || parsed.host_str() != base.host_str()
        || parsed.port_or_known_default() != base.port_or_known_default()
    {
        return Err(DirectoryError::Permanent(
            "a page link left the directory's host".to_owned(),
        ));
    }
    let _permit = egress.admit(limiter).map_err(|e| egress_error(&e))?;
    let resp = egress
        .send_capped(
            client
                .get(parsed)
                .header(reqwest::header::AUTHORIZATION, authorization)
                .header(reqwest::header::ACCEPT, "application/json"),
        )
        .await
        .map_err(|e| egress_error(&e))?;
    let status = resp.status.as_u16();
    if !resp.status.is_success() {
        let why = format!("the directory answered {status}");
        return Err(if status == 429 || status >= 500 {
            DirectoryError::Retryable(why)
        } else {
            DirectoryError::Permanent(why)
        });
    }
    let body = serde_json::from_slice(&resp.body)
        .map_err(|e| DirectoryError::Permanent(format!("the directory's answer: {e}")))?;
    Ok((body, resp.headers))
}

/// Whether `id` can name a directory user in a URL path unencoded: an object
/// id is a GUID; anything with path or query syntax is refused before any
/// request is made.
#[must_use]
pub fn is_object_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(id: &str, name: &str) -> DirectoryGroup {
        DirectoryGroup {
            id: id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn the_name_rule_never_yields_a_reserved_or_privileged_name() {
        let rule = NameRule {
            prefix: Some("escurel-".into()),
            map: [("oid-ops", "ops"), ("oid-x", "admin")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        };
        let reserved = vec!["kb-admins".to_owned()];
        let m = |id: &str, name: &str| rule.apply(&g(id, name), &reserved);
        assert_eq!(m("1", "escurel-sales").as_deref(), Some("sales"));
        assert_eq!(m("2", "Marketing"), None, "unprefixed groups do not count");
        assert_eq!(
            m("oid-ops", "Ops Team").as_deref(),
            Some("ops"),
            "the map wins"
        );
        for bad in [
            "escurel-admin",
            "escurel-public",
            "escurel-owner",
            "escurel-escurel:admin",
            "escurel-kb-admins",
            "escurel-",
        ] {
            assert_eq!(m("3", bad), None, "{bad}");
        }
        assert_eq!(m("oid-x", "anything"), None, "a mapped reserved name too");
        assert!(NameRule::default().is_empty());
    }

    /// A provider answering from a fixed table, or failing.
    struct Fixed {
        name: &'static str,
        groups: Vec<DirectoryGroup>,
        fail: Arc<std::sync::Mutex<Option<DirectoryError>>>,
        by_email: bool,
    }

    impl GroupDirectory for Fixed {
        fn name(&self) -> &'static str {
            self.name
        }
        fn lookup_key(&self, user: &DirectoryUser) -> Option<String> {
            if self.by_email {
                user.email.clone()
            } else {
                user.object_id.clone()
            }
        }
        fn groups_for<'a>(&'a self, _key: &'a str) -> GroupsFuture<'a> {
            let out = match self.fail.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(self.groups.clone()),
            };
            Box::pin(async move { out })
        }
    }

    fn dirs(providers: Vec<Fixed>, ttl: Duration, stale: Duration) -> GroupDirectories {
        GroupDirectories {
            providers: providers
                .into_iter()
                .map(|f| Provider {
                    directory: Box::new(f),
                    rule: NameRule {
                        prefix: Some("e-".into()),
                        map: HashMap::new(),
                    },
                })
                .collect(),
            reserved: vec![],
            cache_ttl: ttl,
            stale_ttl: stale,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn fixed(name: &'static str, groups: &[&str], by_email: bool) -> Fixed {
        Fixed {
            name,
            groups: groups.iter().map(|n| g(n, n)).collect(),
            fail: Arc::new(std::sync::Mutex::new(None)),
            by_email,
        }
    }

    #[tokio::test]
    async fn the_result_is_the_union_and_a_keyless_or_refusing_provider_adds_nothing() {
        let d = dirs(
            vec![
                fixed("graph", &["e-sales", "e-ops"], false),
                fixed("okta", &["e-ops", "e-support"], true),
            ],
            DEFAULT_CACHE_TTL,
            DEFAULT_STALE_TTL,
        );
        let both = DirectoryUser {
            object_id: Some("o".into()),
            email: Some("a@x".into()),
            upn: None,
        };
        assert_eq!(
            d.groups_from_directory(&both).await.unwrap(),
            ["ops", "sales", "support"]
        );
        let no_email = DirectoryUser {
            email: None,
            ..both.clone()
        };
        assert_eq!(
            d.groups_from_directory(&no_email).await.unwrap(),
            ["ops", "sales"],
            "the email-keyed provider adds nothing; the other still answers"
        );

        let refusing = dirs(
            vec![fixed("graph", &["e-sales"], false), {
                let f = fixed("okta", &["e-ops"], true);
                *f.fail.lock().unwrap() = Some(DirectoryError::Permanent("403".into()));
                f
            }],
            DEFAULT_CACHE_TTL,
            DEFAULT_STALE_TTL,
        );
        assert_eq!(
            refusing.groups_from_directory(&both).await.unwrap(),
            ["sales"]
        );
    }

    #[tokio::test]
    async fn a_retryable_failure_uses_the_cache_until_it_is_too_old() {
        let provider = fixed("graph", &["e-sales"], false);
        let fail = Arc::clone(&provider.fail);
        let d = dirs(vec![provider], Duration::ZERO, Duration::from_millis(200));
        let user = DirectoryUser {
            object_id: Some("o".into()),
            ..Default::default()
        };
        assert_eq!(d.groups_from_directory(&user).await.unwrap(), ["sales"]);
        *fail.lock().unwrap() = Some(DirectoryError::Retryable("503".into()));
        assert_eq!(d.groups_from_directory(&user).await.unwrap(), ["sales"]);
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(matches!(
            d.groups_from_directory(&user).await,
            Err(DirectoryError::Retryable(_))
        ));
    }
}
