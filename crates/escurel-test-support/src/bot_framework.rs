//! [`BotFrameworkStandIn`] — Microsoft's side of a Teams bot, as a local HTTP
//! server: the OpenID metadata and JWKS a channel token is verified against
//! (a test RSA key, published under a Teams-endorsed kid and a kid endorsed
//! only for another channel), the client-credentials token endpoint, the
//! conversation-members API (a roster a test edits) and the connector that
//! records every activity posted to it.
//!
//! Both the chat bridge (which verifies inbound activities and replies) and
//! the gateway (which, with `ESCUREL_AUTH_TEAMS_*`, verifies the forwarded
//! activity token and asks the roster itself, #835) talk to it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::ExtraIssuer;
use crate::directory::percent_decode;

/// The `iss` of every channel token the stand-in signs.
pub const BF_ISSUER: &str = "https://api.botframework.com";
/// The kid whose JWKS entry is endorsed for `msteams`.
pub const TEAMS_KID: &str = "bf-teams";
/// The same key under a kid endorsed only for `webchat`.
pub const WEBCHAT_KID: &str = "bf-webchat";
/// What the token endpoint hands out.
pub const CONNECTOR_TOKEN: &str = "connector-token";

/// One person on the roster, as the members API answers for them.
#[derive(Debug, Clone)]
pub struct RosterMember {
    /// `None` answers without `aadObjectId` (a guest or an anonymous user).
    pub aad_object_id: Option<String>,
    pub email: String,
    /// `None` answers without `tenantId`.
    pub tenant_id: Option<String>,
}

/// What the stand-in saw, and how a test bends it.
#[derive(Default)]
pub struct Seen {
    /// `(path, authorization, body)` per activity posted to the connector.
    pub activities: Vec<(String, String, Value)>,
    /// Form bodies posted to the token endpoint.
    pub token_requests: Vec<String>,
    /// Record a `message` activity, then drop the connection without an
    /// answer: the reply was delivered, but the sender cannot know it.
    pub drop_after_recording: bool,
    /// Answer the conversation-members API with this status instead.
    pub member_status: Option<u16>,
    /// How often the conversation-members API was asked.
    pub member_calls: usize,
    /// The `authorization` header of every members-API request.
    pub member_authorizations: Vec<String>,
    /// Channel user id (`29:…`) → who they are.
    pub roster: HashMap<String, RosterMember>,
    /// `(conversation id, user id)` pairs the roster does NOT hold (the
    /// person is known, but not in that conversation).
    pub not_in: HashSet<(String, String)>,
    /// Answer the token endpoint with this status instead.
    pub token_status: Option<u16>,
    /// How often `/jwks` was fetched.
    pub jwks_calls: usize,
    /// The `/jwks` fetches (by 0-based index) answered `503`.
    pub jwks_fail: std::ops::Range<usize>,
}

/// A running Bot Framework stand-in.
pub struct BotFrameworkStandIn {
    /// `http://127.0.0.1:<port>`: the `serviceUrl` its tokens are bound to.
    pub base: String,
    pub seen: Arc<Mutex<Seen>>,
    issuer: ExtraIssuer,
}

impl BotFrameworkStandIn {
    pub async fn start() -> Self {
        let issuer = ExtraIssuer::start().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the Bot Framework stand-in");
        let base = format!("http://{}", listener.local_addr().expect("local addr"));
        let metadata = json!({ "issuer": BF_ISSUER, "jwks_uri": format!("{base}/jwks"),
            "id_token_signing_alg_values_supported": ["RS256"] });
        let mut teams_key = issuer.public_jwk(TEAMS_KID);
        teams_key["endorsements"] = json!(["msteams"]);
        let mut webchat_key = issuer.public_jwk(WEBCHAT_KID);
        webchat_key["endorsements"] = json!(["webchat"]);
        let jwks = json!({ "keys": [teams_key, webchat_key] });

        let app = Router::new()
            .route(
                "/openid",
                get(move || {
                    let m = metadata.clone();
                    async move { axum::Json(m) }
                }),
            )
            .route(
                "/jwks",
                get(move |State(seen): State<Arc<Mutex<Seen>>>| {
                    let k = jwks.clone();
                    async move {
                        let mut seen = seen.lock().unwrap();
                        let n = seen.jwks_calls;
                        seen.jwks_calls += 1;
                        if seen.jwks_fail.contains(&n) {
                            return Err(StatusCode::SERVICE_UNAVAILABLE);
                        }
                        Ok(axum::Json(k))
                    }
                }),
            )
            .route("/token", post(token))
            .route("/v3/conversations/{*rest}", post(activity).get(member))
            .route(
                "/files/memo.txt",
                get(|| async { "Globex renews in 2028.\n" }),
            )
            .with_state(Arc::clone(&seen));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { base, seen, issuer }
    }

    /// `/openid`: the metadata URL a verifier is configured with.
    #[must_use]
    pub fn openid_url(&self) -> String {
        format!("{}/openid", self.base)
    }

    /// `/token`: the client-credentials endpoint.
    #[must_use]
    pub fn token_url(&self) -> String {
        format!("{}/token", self.base)
    }

    /// The gateway's `ESCUREL_AUTH_TEAMS_*` for the bot `app_id`, pointed at
    /// this stand-in (the one allow-listed connector host: `127.0.0.1`).
    /// Set it as `BridgeMintConfig::teams`.
    #[must_use]
    pub fn gateway_verification(
        &self,
        app_id: &str,
    ) -> escurel_server::teams_verify::TeamsVerifyConfig {
        let mut cfg = escurel_server::teams_verify::TeamsVerifyConfig::new(app_id, "bot-secret");
        cfg.openid_metadata_url = self.openid_url();
        cfg.issuer = BF_ISSUER.to_owned();
        cfg.token_url = self.token_url();
        cfg.service_url_hosts = vec!["127.0.0.1".to_owned()];
        cfg.jwks_refresh_cooldown = std::time::Duration::ZERO;
        cfg
    }

    /// Put `user_id` on the roster of every conversation.
    pub fn add_member(&self, user_id: &str, aad_object_id: &str, email: &str, tenant_id: &str) {
        self.add_roster_member(
            user_id,
            RosterMember {
                aad_object_id: Some(aad_object_id.to_owned()),
                email: email.to_owned(),
                tenant_id: Some(tenant_id.to_owned()),
            },
        );
    }

    /// Put `user_id` on every roster as exactly `member` says.
    pub fn add_roster_member(&self, user_id: &str, member: RosterMember) {
        self.seen
            .lock()
            .unwrap()
            .roster
            .insert(user_id.to_owned(), member);
    }

    /// Take `user_id` off `conversation_id`'s roster (they stay on others).
    pub fn remove_from(&self, conversation_id: &str, user_id: &str) {
        self.seen
            .lock()
            .unwrap()
            .not_in
            .insert((conversation_id.to_owned(), user_id.to_owned()));
    }

    /// A channel token as the Bot Framework signs it for the bot `audience`,
    /// bound to this stand-in's `serviceUrl`, valid for ten minutes.
    #[must_use]
    pub fn token(&self, audience: &str) -> String {
        self.token_with_kid(audience, TEAMS_KID)
    }

    #[must_use]
    pub fn token_with_kid(&self, audience: &str, kid: &str) -> String {
        let now = now_secs();
        self.sign(
            &json!({
                "iss": BF_ISSUER, "aud": audience, "serviceurl": self.base,
                "nbf": now - 5, "iat": now - 5, "exp": now + 600,
            }),
            kid,
        )
    }

    /// Sign arbitrary channel-token `claims` under `kid` (an expired token,
    /// another `serviceurl`, …).
    #[must_use]
    pub fn sign(&self, claims: &Value, kid: &str) -> String {
        self.issuer.sign_with_kid(claims, kid)
    }

    /// The `message` activities posted to the connector (typing excluded), in order.
    #[must_use]
    pub fn replies(&self) -> Vec<(String, String, Value)> {
        self.seen
            .lock()
            .unwrap()
            .activities
            .iter()
            .filter(|(_, _, b)| b["type"] == "message")
            .cloned()
            .collect()
    }

    /// Every activity's `type`, in order.
    #[must_use]
    pub fn activity_types(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .activities
            .iter()
            .map(|(_, _, b)| b["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn token(
    State(seen): State<Arc<Mutex<Seen>>>,
    body: String,
) -> Result<axum::Json<Value>, StatusCode> {
    let mut seen = seen.lock().unwrap();
    seen.token_requests.push(body);
    if let Some(status) = seen.token_status {
        return Err(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    }
    Ok(axum::Json(
        json!({ "token_type": "Bearer", "expires_in": 3600,
                          "access_token": CONNECTOR_TOKEN }),
    ))
}

/// The raw path: `Path` would percent-decode the conversation id.
async fn activity(
    State(seen): State<Arc<Mutex<Seen>>>,
    uri: Uri,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    let rest = uri
        .path()
        .strip_prefix("/v3/conversations/")
        .unwrap_or_default()
        .to_owned();
    let drop_it = {
        let mut seen = seen.lock().unwrap();
        let drop_it = seen.drop_after_recording && body["type"] == "message";
        seen.activities.push((rest, authorization(&headers), body));
        drop_it
    };
    // A panic ends this connection's task: the client sees the connection
    // close after its request was read, with no response.
    assert!(!drop_it, "stand-in connector: dropping the connection");
    axum::Json(json!({ "id": "sent-1" }))
}

/// `GET /v3/conversations/{conversation}/members/{user}`: the roster.
async fn member(
    State(seen): State<Arc<Mutex<Seen>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<axum::Json<Value>, StatusCode> {
    let mut seen = seen.lock().unwrap();
    seen.member_calls += 1;
    seen.member_authorizations.push(authorization(&headers));
    if let Some(status) = seen.member_status {
        return Err(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    }
    if authorization(&headers) != format!("Bearer {CONNECTOR_TOKEN}") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rest = uri
        .path()
        .strip_prefix("/v3/conversations/")
        .unwrap_or_default();
    let Some((conversation, user)) = rest.split_once("/members/") else {
        return Err(StatusCode::NOT_FOUND);
    };
    // Both are percent-encoded as one path segment each.
    let (conversation, user) = (percent_decode(conversation), percent_decode(user));
    if seen.not_in.contains(&(conversation, user.clone())) {
        return Err(StatusCode::NOT_FOUND);
    }
    let m = seen.roster.get(&user).ok_or(StatusCode::NOT_FOUND)?;
    let mut answer = json!({ "id": user, "name": "Member", "email": m.email,
                             "userPrincipalName": m.email });
    if let Some(aad) = &m.aad_object_id {
        answer["aadObjectId"] = json!(aad);
    }
    if let Some(tenant) = &m.tenant_id {
        answer["tenantId"] = json!(tenant);
    }
    Ok(axum::Json(answer))
}

fn authorization(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}
