//! [`DirectoryStandIn`] — the identity provider's side of the gateway's group
//! lookup (#830), as a local HTTP server, for each of Microsoft Graph, Google
//! Cloud Identity and Okta: the token endpoint (where the provider has one)
//! and the group listing, paged one group per page the way each provider
//! pages (`@odata.nextLink`, `nextPageToken`, `Link: rel="next"`).
//!
//! Everything escurel does with the answer is real (the egress policy, the
//! token and membership caches, the name rule, the union); only the provider
//! is stood in for. A test can make it answer any status (`429` / `503` are
//! retryable, `401` / `403` / `404` permanent) and put it back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use escurel_server::group_directory::{
    BridgeMintConfig, DirectoryConfig, NameRule, google, graph, okta,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The Entra tenant the Graph stand-in serves.
pub const GRAPH_TENANT: &str = "entra-tenant";
const GOOGLE_SA: &str = "escurel-groups@test-project.iam.gserviceaccount.com";
const OKTA_TOKEN: &str = "okta-api-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Graph,
    Google,
    Okta,
}

#[derive(Default)]
struct State {
    /// lookup key (object id or email) → group names.
    members: HashMap<String, Vec<String>>,
    /// Answer every request with this status instead.
    status: Option<u16>,
    lookups: usize,
}

/// A running provider stand-in.
pub struct DirectoryStandIn {
    kind: Kind,
    server: MockServer,
    state: Arc<Mutex<State>>,
    /// The Google service-account key the stand-in accepts assertions from.
    google_key_pem: Option<String>,
}

impl DirectoryStandIn {
    pub async fn graph() -> Self {
        Self::start(Kind::Graph).await
    }

    pub async fn google() -> Self {
        Self::start(Kind::Google).await
    }

    pub async fn okta() -> Self {
        Self::start(Kind::Okta).await
    }

    async fn start(kind: Kind) -> Self {
        let server = MockServer::start().await;
        let state = Arc::new(Mutex::new(State::default()));
        let base = server.uri();
        let token_path = match kind {
            Kind::Graph => Some(format!("/{GRAPH_TENANT}/oauth2/v2.0/token")),
            Kind::Google => Some("/token".to_owned()),
            Kind::Okta => None,
        };
        if let Some(token_path) = token_path {
            let s = Arc::clone(&state);
            Mock::given(method("POST"))
                .and(path(token_path))
                .respond_with(move |req: &Request| {
                    if let Some(status) = s.lock().unwrap().status {
                        return ResponseTemplate::new(status);
                    }
                    let form = String::from_utf8_lossy(&req.body).into_owned();
                    let ok = match kind {
                        Kind::Graph => {
                            form.contains("grant_type=client_credentials")
                                && form
                                    .contains("scope=https%3A%2F%2Fgraph.microsoft.com%2F.default")
                        }
                        _ => {
                            form.contains(
                                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
                            ) && form.contains("assertion=ey")
                        }
                    };
                    if !ok {
                        return ResponseTemplate::new(400);
                    }
                    ResponseTemplate::new(200).set_body_json(json!({
                        "token_type": "Bearer", "expires_in": 3600, "access_token": "dir-token"
                    }))
                })
                .mount(&server)
                .await;
        }
        let listing = match kind {
            Kind::Graph => r"^/v1\.0/users/[^/]+/transitiveMemberOf/microsoft\.graph\.group$",
            Kind::Google => r"^/v1/groups/-/memberships:searchTransitiveGroups$",
            Kind::Okta => r"^/api/v1/users/[^/]+/groups$",
        };
        let s = Arc::clone(&state);
        Mock::given(method("GET"))
            .and(path_regex(listing))
            .respond_with(move |req: &Request| listing_page(kind, &base, &s, req))
            .mount(&server)
            .await;
        let google_key_pem = (kind == Kind::Google).then(|| {
            use rsa::pkcs8::EncodePrivateKey;
            rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
                .expect("rsa key")
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .expect("pem")
                .to_string()
        });
        Self {
            kind,
            server,
            state,
            google_key_pem,
        }
    }

    /// `key` (an object id for Graph, an email for Google and Okta) is a
    /// member of these groups, by name (a Google group's name is its email
    /// local part: `escurel-sales` stands for `escurel-sales@acme.example`).
    pub fn member_of(&self, key: &str, groups: &[&str]) {
        self.state.lock().unwrap().members.insert(
            key.to_owned(),
            groups.iter().map(|g| (*g).to_owned()).collect(),
        );
    }

    /// Answer everything with `status` until [`Self::up`].
    pub fn answer_with(&self, status: u16) {
        self.state.lock().unwrap().status = Some(status);
    }

    /// The provider is unreachable in the retryable way (`503`).
    pub fn down(&self) {
        self.answer_with(503);
    }

    pub fn up(&self) {
        self.state.lock().unwrap().status = None;
    }

    /// How many listing pages were asked for.
    pub fn lookups(&self) -> usize {
        self.state.lock().unwrap().lookups
    }

    /// The gateway's provider configuration against this stand-in, counting
    /// the groups named with `prefix`.
    pub fn config(&self, prefix: &str) -> DirectoryConfig {
        let rule = NameRule {
            prefix: Some(prefix.to_owned()),
            map: HashMap::new(),
        };
        let base = self.server.uri();
        match self.kind {
            Kind::Graph => DirectoryConfig::Graph(graph::GraphConfig {
                tenant_id: GRAPH_TENANT.to_owned(),
                client_id: "graph-app".to_owned(),
                client_secret: "not-a-real-secret".to_owned(),
                token_url: format!("{base}/{GRAPH_TENANT}/oauth2/v2.0/token"),
                base_url: base,
                rule,
            }),
            Kind::Google => DirectoryConfig::Google(google::GoogleConfig {
                client_email: GOOGLE_SA.to_owned(),
                private_key: self.google_key_pem.clone().expect("google key"),
                private_key_id: Some("k1".to_owned()),
                subject: Some("admin@acme.example".to_owned()),
                token_url: format!("{base}/token"),
                base_url: base,
                group_name: google::GroupName::EmailLocal,
                rule,
            }),
            Kind::Okta => DirectoryConfig::Okta(okta::OktaConfig {
                base_url: base,
                api_token: OKTA_TOKEN.to_owned(),
                user_key: okta::UserKey::Email,
                rule,
            }),
        }
    }
}

/// The gateway's `mint_user_token` configuration over `directories`, with
/// the membership cache's fresh and stale windows (10 min / 1 h in
/// production) — shorten them to watch both run out.
#[must_use]
pub fn bridge_mint(
    template: &str,
    directories: Vec<DirectoryConfig>,
    cache: Option<(Duration, Duration)>,
) -> BridgeMintConfig {
    let (cache_ttl, stale_ttl) = cache.unwrap_or((
        escurel_server::group_directory::DEFAULT_CACHE_TTL,
        escurel_server::group_directory::DEFAULT_STALE_TTL,
    ));
    BridgeMintConfig {
        principal_template: template.to_owned(),
        directories,
        cache_ttl,
        stale_ttl,
    }
}

/// One listing page, as the provider would answer it.
fn listing_page(kind: Kind, base: &str, s: &Mutex<State>, req: &Request) -> ResponseTemplate {
    let mut st = s.lock().unwrap();
    st.lookups += 1;
    if let Some(status) = st.status {
        return ResponseTemplate::new(status);
    }
    let auth = req
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    let expected = match kind {
        Kind::Okta => format!("SSWS {OKTA_TOKEN}"),
        _ => "Bearer dir-token".to_owned(),
    };
    if auth != Some(expected.as_str()) {
        return ResponseTemplate::new(401);
    }
    let query = |k: &str| {
        req.url
            .query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let segment = |i: usize| {
        req.url
            .path_segments()
            .and_then(|mut s| s.nth(i))
            .map(percent_decode)
            .unwrap_or_default()
    };
    let key = match kind {
        Kind::Graph => segment(2),
        Kind::Okta => segment(3),
        Kind::Google => query("query")
            .and_then(|q| {
                let rest = q.split_once("member_key_id=='")?.1;
                Some(rest.split_once('\'')?.0.to_owned())
            })
            .unwrap_or_default(),
    };
    let Some(groups) = st.members.get(&key).cloned() else {
        return match kind {
            // Graph and Okta know their users; Google simply finds no groups.
            Kind::Google => ResponseTemplate::new(200).set_body_json(json!({ "memberships": [] })),
            _ => ResponseTemplate::new(404),
        };
    };
    let at: usize = query(match kind {
        Kind::Graph => "$skiptoken",
        Kind::Google => "pageToken",
        Kind::Okta => "after",
    })
    .and_then(|v| v.parse().ok())
    .unwrap_or(0);
    let more = at + 1 < groups.len();
    let page: Vec<&String> = groups.iter().skip(at).take(1).collect();
    let path = req.url.path();
    match kind {
        Kind::Graph => {
            let value: Vec<Value> = page
                .iter()
                .map(|g| json!({ "@odata.type": "#microsoft.graph.group", "id": format!("oid-{g}"), "displayName": g }))
                .collect();
            let mut body = json!({ "value": value });
            if more {
                body["@odata.nextLink"] = json!(format!(
                    "{base}{path}?$select=id,displayName&$skiptoken={}",
                    at + 1
                ));
            }
            ResponseTemplate::new(200).set_body_json(body)
        }
        Kind::Google => {
            let memberships: Vec<Value> = page
                .iter()
                .map(|g| {
                    json!({
                        "group": format!("groups/{g}"),
                        "groupKey": { "id": format!("{g}@acme.example") },
                        "displayName": g,
                        "labels": { "cloudidentity.googleapis.com/groups.discussion_forum": "" },
                    })
                })
                .collect();
            let mut body = json!({ "memberships": memberships });
            if more {
                body["nextPageToken"] = json!((at + 1).to_string());
            }
            ResponseTemplate::new(200).set_body_json(body)
        }
        Kind::Okta => {
            let body: Vec<Value> = page
                .iter()
                .map(|g| json!({ "id": format!("00g-{g}"), "profile": { "name": g } }))
                .collect();
            let mut resp = ResponseTemplate::new(200).set_body_json(body);
            if more {
                resp = resp.insert_header(
                    "link",
                    format!("<{base}{path}?after={}&limit=200>; rel=\"next\"", at + 1).as_str(),
                );
            }
            resp
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
