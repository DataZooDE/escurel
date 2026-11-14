//! The group directories' `ESCUREL_AUTH_*` configuration (#830), validated
//! whole at boot by [`bridge_mint_from`]. Lives next to the providers it
//! configures; [`crate::config`] calls it with the verifier and signing
//! settings it already parsed.

use crate::config::{AuthConfig, ConfigError, EnvSource, SigningConfig, secret_or_file};

/// Each group directory's keys (#830): setting one names that provider.
const DIRECTORY_KEYS: &[(&str, &[&str])] = &[
    (
        "graph",
        &[
            "ESCUREL_AUTH_GRAPH_TENANT_ID",
            "ESCUREL_AUTH_GRAPH_CLIENT_ID",
            "ESCUREL_AUTH_GRAPH_CLIENT_SECRET",
            "ESCUREL_AUTH_GRAPH_CLIENT_SECRET_FILE",
            "ESCUREL_AUTH_GRAPH_TOKEN_URL",
            "ESCUREL_AUTH_GRAPH_BASE_URL",
            "ESCUREL_AUTH_GRAPH_GROUP_PREFIX",
            "ESCUREL_AUTH_GRAPH_GROUP_MAP_FILE",
            "ESCUREL_AUTH_GRAPH_INCLUDE_M365_GROUPS",
        ],
    ),
    (
        "google",
        &[
            "ESCUREL_AUTH_GOOGLE_SA_KEY_FILE",
            "ESCUREL_AUTH_GOOGLE_SUBJECT",
            "ESCUREL_AUTH_GOOGLE_TOKEN_URL",
            "ESCUREL_AUTH_GOOGLE_BASE_URL",
            "ESCUREL_AUTH_GOOGLE_GROUP_NAME",
            "ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN",
            "ESCUREL_AUTH_GOOGLE_GROUP_PREFIX",
            "ESCUREL_AUTH_GOOGLE_GROUP_MAP_FILE",
        ],
    ),
    (
        "okta",
        &[
            "ESCUREL_AUTH_OKTA_BASE_URL",
            "ESCUREL_AUTH_OKTA_API_TOKEN",
            "ESCUREL_AUTH_OKTA_API_TOKEN_FILE",
            "ESCUREL_AUTH_OKTA_USER_KEY",
            "ESCUREL_AUTH_OKTA_GROUP_PREFIX",
            "ESCUREL_AUTH_OKTA_GROUP_MAP_FILE",
            "ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS",
        ],
    ),
];

pub(crate) fn bridge_refusal<T>(why: String) -> Result<T, ConfigError> {
    Err(ConfigError::Bridge(why))
}

/// An https URL, or plain http to a loopback address (tests, local development).
fn directory_url(key: &str, url: &str) -> Result<(), ConfigError> {
    let ok = reqwest::Url::parse(url).is_ok_and(|u| {
        u.host_str().is_some()
            && (u.scheme() == "https"
                || (u.scheme() == "http"
                    && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
    });
    if ok {
        Ok(())
    } else {
        bridge_refusal(format!(
            "{key} `{url}` must be an https URL (plain http only to a loopback address)"
        ))
    }
}

/// An opt-in that widens which directory groups count: exactly `true` or
/// `false` (unset = `false`), so a typo never widens anything silently.
fn directory_flag(env: &dyn EnvSource, key: &str) -> Result<bool, ConfigError> {
    match env
        .get(key)
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => bridge_refusal(format!("{key} `{other}` is `true` or `false`")),
    }
}

/// A provider's `<P>_GROUP_PREFIX` / `<P>_GROUP_MAP_FILE`: at least one, and a
/// map that names only assignable, non-role groups.
fn directory_rule(
    env: &dyn EnvSource,
    prefix_key: &str,
    map_key: &str,
    reserved: &[&str],
) -> Result<crate::group_directory::NameRule, ConfigError> {
    let get = |k: &str| env.get(k).filter(|v| !v.trim().is_empty());
    let prefix = get(prefix_key);
    let map = match get(map_key) {
        None => std::collections::HashMap::new(),
        Some(path) => {
            let raw = std::fs::read_to_string(&path)
                .or_else(|e| bridge_refusal(format!("{map_key} `{path}` cannot be read: {e}")))?;
            let map: std::collections::HashMap<String, String> = serde_json::from_str(&raw)
                .or_else(|e| {
                    bridge_refusal(format!(
                        "{map_key} `{path}` is not a JSON object of directory group id → \
                         escurel group: {e}"
                    ))
                })?;
            if let Some((id, group)) = map.iter().find(|(_, g)| {
                !escurel_types::chat_channel::is_assignable_group(g)
                    || reserved.contains(&g.as_str())
            }) {
                return bridge_refusal(format!(
                    "{map_key} maps `{id}` to `{group}`, which cannot be a group (reserved, a \
                     role value, or not [A-Za-z0-9._@-])"
                ));
            }
            map
        }
    };
    let rule = crate::group_directory::NameRule { prefix, map };
    if rule.is_empty() {
        return bridge_refusal(format!(
            "set {prefix_key} and/or {map_key}: without a mapping rule no directory group \
             becomes an escurel group"
        ));
    }
    Ok(rule)
}

/// `mint_user_token`'s configuration (#830), validated whole at boot: a
/// principal template that could merge two people, a listed directory with a
/// part missing, a provider's keys set while it is not listed, a mapping that
/// yields nothing or a reserved name, or a gateway with no verifier or
/// signing identity to mint with, refuses to start rather than refuse every
/// turn later.
pub(crate) fn bridge_mint_from(
    env: &dyn EnvSource,
    auth: Option<&AuthConfig>,
    signing: Option<&SigningConfig>,
) -> Result<Option<crate::group_directory::BridgeMintConfig>, ConfigError> {
    use crate::group_directory::{BridgeMintConfig, DirectoryConfig, google, graph, okta};
    let get = |k: &str| env.get(k).filter(|v| !v.trim().is_empty());
    let set_key = |provider: &str| {
        DIRECTORY_KEYS
            .iter()
            .find(|(p, _)| *p == provider)
            .and_then(|(_, keys)| keys.iter().find(|k| get(k).is_some()).copied())
    };
    let Some(template) = get("ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE") else {
        let stray = get("ESCUREL_AUTH_GROUP_DIRECTORIES")
            .map(|_| "ESCUREL_AUTH_GROUP_DIRECTORIES")
            .or_else(|| DIRECTORY_KEYS.iter().find_map(|(p, _)| set_key(p)));
        return match stray {
            Some(k) => bridge_refusal(format!(
                "{k} is set but ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE is not: the group \
                 directories serve the bridge's mint, which needs the template"
            )),
            None => Ok(None),
        };
    };
    escurel_types::bridge::validate_principal_template(&template)
        .or_else(|why| bridge_refusal(format!("ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE: {why}")))?;
    let Some(auth) = auth else {
        return bridge_refusal(
            "ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE needs an OIDC verifier (ESCUREL_AUTH_OIDC_ISSUER)"
                .to_owned(),
        );
    };
    if signing.is_none() {
        return bridge_refusal(
            "ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE needs a signing identity \
             (ESCUREL_AUTH_SIGNING_KEY): the gateway mints the asker's token"
                .to_owned(),
        );
    }
    if auth.groups_claim != "roles" {
        return bridge_refusal(format!(
            "ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE needs ESCUREL_AUTH_GROUPS_CLAIM `roles` (it is \
             `{}`): a minted turn carries its groups there",
            auth.groups_claim
        ));
    }
    let listed: Vec<String> = get("ESCUREL_AUTH_GROUP_DIRECTORIES")
        .unwrap_or_default()
        .split(',')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if listed.is_empty() {
        tracing::info!(
            "ESCUREL_AUTH_GROUP_DIRECTORIES is unset: the chat bridge's 1:1 turns carry no \
             groups, and group chats and channels are not answered"
        );
    }
    for (i, p) in listed.iter().enumerate() {
        if !DIRECTORY_KEYS.iter().any(|(known, _)| known == p) {
            return bridge_refusal(format!(
                "ESCUREL_AUTH_GROUP_DIRECTORIES names `{p}`; known: graph, google, okta"
            ));
        }
        if listed[..i].contains(p) {
            return bridge_refusal(format!("ESCUREL_AUTH_GROUP_DIRECTORIES names `{p}` twice"));
        }
    }
    for (p, _) in DIRECTORY_KEYS {
        if !listed.iter().any(|l| l == p)
            && let Some(k) = set_key(p)
        {
            return bridge_refusal(format!(
                "{k} is set but `{p}` is not in ESCUREL_AUTH_GROUP_DIRECTORIES"
            ));
        }
    }
    let required = |k: &'static str| {
        get(k).ok_or_else(|| {
            ConfigError::Bridge(format!(
                "{k} is required while ESCUREL_AUTH_GROUP_DIRECTORIES lists its provider"
            ))
        })
    };
    let reserved = auth.builtin_role_values();
    let mut directories = Vec::with_capacity(listed.len());
    for p in &listed {
        directories.push(match p.as_str() {
            "graph" => {
                let tenant_id = required("ESCUREL_AUTH_GRAPH_TENANT_ID")?;
                let client_secret = secret_or_file(
                    "ESCUREL_AUTH_GRAPH_CLIENT_SECRET_FILE",
                    "set either ESCUREL_AUTH_GRAPH_CLIENT_SECRET or …_FILE, not both",
                    get("ESCUREL_AUTH_GRAPH_CLIENT_SECRET"),
                    get("ESCUREL_AUTH_GRAPH_CLIENT_SECRET_FILE"),
                )?
                .ok_or_else(|| {
                    ConfigError::Bridge(
                        "ESCUREL_AUTH_GRAPH_CLIENT_SECRET (or …_FILE) is required for `graph`"
                            .to_owned(),
                    )
                })?;
                let token_url = get("ESCUREL_AUTH_GRAPH_TOKEN_URL")
                    .unwrap_or_else(|| graph::default_token_url(&tenant_id));
                let base_url = get("ESCUREL_AUTH_GRAPH_BASE_URL")
                    .unwrap_or_else(|| graph::DEFAULT_BASE_URL.to_owned());
                directory_url("ESCUREL_AUTH_GRAPH_TOKEN_URL", &token_url)?;
                directory_url("ESCUREL_AUTH_GRAPH_BASE_URL", &base_url)?;
                DirectoryConfig::Graph(graph::GraphConfig {
                    client_id: required("ESCUREL_AUTH_GRAPH_CLIENT_ID")?,
                    tenant_id,
                    client_secret,
                    token_url,
                    base_url,
                    rule: directory_rule(
                        env,
                        "ESCUREL_AUTH_GRAPH_GROUP_PREFIX",
                        "ESCUREL_AUTH_GRAPH_GROUP_MAP_FILE",
                        &reserved,
                    )?,
                    include_m365_groups: directory_flag(
                        env,
                        "ESCUREL_AUTH_GRAPH_INCLUDE_M365_GROUPS",
                    )?,
                })
            }
            "google" => {
                let path = required("ESCUREL_AUTH_GOOGLE_SA_KEY_FILE")?;
                let key: serde_json::Value = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .ok_or_else(|| {
                        ConfigError::Bridge(format!(
                            "ESCUREL_AUTH_GOOGLE_SA_KEY_FILE `{path}` is not a readable \
                             service-account key (JSON)"
                        ))
                    })?;
                let field = |k: &str| key[k].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
                let (Some(client_email), Some(private_key)) =
                    (field("client_email"), field("private_key"))
                else {
                    return bridge_refusal(format!(
                        "ESCUREL_AUTH_GOOGLE_SA_KEY_FILE `{path}` has no `client_email` / \
                         `private_key`"
                    ));
                };
                let token_url = get("ESCUREL_AUTH_GOOGLE_TOKEN_URL")
                    .or_else(|| field("token_uri"))
                    .unwrap_or_else(|| google::DEFAULT_TOKEN_URL.to_owned());
                let base_url = get("ESCUREL_AUTH_GOOGLE_BASE_URL")
                    .unwrap_or_else(|| google::DEFAULT_BASE_URL.to_owned());
                directory_url("ESCUREL_AUTH_GOOGLE_TOKEN_URL", &token_url)?;
                directory_url("ESCUREL_AUTH_GOOGLE_BASE_URL", &base_url)?;
                let group_domains: Vec<String> = get("ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN")
                    .unwrap_or_default()
                    .split(',')
                    .map(|d| d.trim().to_ascii_lowercase())
                    .filter(|d| !d.is_empty())
                    .collect();
                let group_name = match get("ESCUREL_AUTH_GOOGLE_GROUP_NAME").as_deref() {
                    None | Some("email") => google::GroupName::Email,
                    Some("email_local") if group_domains.is_empty() => {
                        return bridge_refusal(
                            "ESCUREL_AUTH_GOOGLE_GROUP_NAME `email_local` needs \
                             ESCUREL_AUTH_GOOGLE_GROUP_DOMAIN: without it `sales@` in any \
                             domain would be the same group"
                                .to_owned(),
                        );
                    }
                    Some("email_local") => google::GroupName::EmailLocal,
                    Some(other) => {
                        return bridge_refusal(format!(
                            "ESCUREL_AUTH_GOOGLE_GROUP_NAME `{other}` is `email` or `email_local`"
                        ));
                    }
                };
                let cfg = google::GoogleConfig {
                    client_email,
                    private_key,
                    private_key_id: field("private_key_id"),
                    subject: get("ESCUREL_AUTH_GOOGLE_SUBJECT"),
                    token_url,
                    base_url,
                    group_name,
                    group_domains,
                    rule: directory_rule(
                        env,
                        "ESCUREL_AUTH_GOOGLE_GROUP_PREFIX",
                        "ESCUREL_AUTH_GOOGLE_GROUP_MAP_FILE",
                        &reserved,
                    )?,
                };
                // Refuse a key that cannot sign now, not at the first turn.
                cfg.credentials().or_else(bridge_refusal)?;
                DirectoryConfig::Google(cfg)
            }
            _ => {
                let base_url = required("ESCUREL_AUTH_OKTA_BASE_URL")?;
                directory_url("ESCUREL_AUTH_OKTA_BASE_URL", &base_url)?;
                let api_token = secret_or_file(
                    "ESCUREL_AUTH_OKTA_API_TOKEN_FILE",
                    "set either ESCUREL_AUTH_OKTA_API_TOKEN or …_FILE, not both",
                    get("ESCUREL_AUTH_OKTA_API_TOKEN"),
                    get("ESCUREL_AUTH_OKTA_API_TOKEN_FILE"),
                )?
                .ok_or_else(|| {
                    ConfigError::Bridge(
                        "ESCUREL_AUTH_OKTA_API_TOKEN (or …_FILE) is required for `okta`".to_owned(),
                    )
                })?;
                let user_key = match get("ESCUREL_AUTH_OKTA_USER_KEY").as_deref() {
                    None | Some("email") => okta::UserKey::Email,
                    Some("upn") => okta::UserKey::Upn,
                    Some(other) => {
                        return bridge_refusal(format!(
                            "ESCUREL_AUTH_OKTA_USER_KEY `{other}` is `email` or `upn`"
                        ));
                    }
                };
                DirectoryConfig::Okta(okta::OktaConfig {
                    base_url,
                    api_token,
                    user_key,
                    rule: directory_rule(
                        env,
                        "ESCUREL_AUTH_OKTA_GROUP_PREFIX",
                        "ESCUREL_AUTH_OKTA_GROUP_MAP_FILE",
                        &reserved,
                    )?,
                    include_app_groups: directory_flag(
                        env,
                        "ESCUREL_AUTH_OKTA_INCLUDE_APP_GROUPS",
                    )?,
                })
            }
        });
    }
    Ok(Some(BridgeMintConfig {
        principal_template: template,
        directories,
        cache_ttl: crate::group_directory::DEFAULT_CACHE_TTL,
        stale_ttl: crate::group_directory::DEFAULT_STALE_TTL,
    }))
}
