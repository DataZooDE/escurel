//! The Teams chat bridge end to end (#824, #830): a Bot Framework message
//! activity in, exactly one reply out, the turn's history under its subject's
//! chat group.
//!
//! escurel is real throughout: the gateway (`EscurelProcess`, write ACL
//! enforced, minting each turn's token for the bridge's `escurel:bridge`
//! credential with the asker's groups from the configured group directories),
//! the runner binary in minted mode on the echo harness (it answers a chat
//! turn with `echo: <the question>`, and `read: <page>` with whether the run's
//! token reads it), and the `escurel-chat-bridge` binary. Only the outside
//! world is a stand-in: Microsoft's Bot Framework, served from this file (the
//! OpenID metadata + JWKS the channel token is verified against, the
//! client-credentials token endpoint, the conversation-members API and the
//! connector that records every activity the bridge sends back), and the group
//! directories (`DirectoryStandIn`: Microsoft Graph, Google Cloud Identity,
//! Okta). The deprecated self-signed bridge key is exercised by one test.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::{get, post};
use escurel_test_support::{
    AuthMode, ConfigOverrides, DirectoryStandIn, EscurelProcess, ExtraIssuer, FixtureBuilder,
    GRAPH_TENANT, Opts, Role, RunnerProcess, RunningRunner, WriteAclMode, bridge_mint, free_port,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const APP_ID: &str = "test-bot-app";
const BF_ISSUER: &str = "https://api.botframework.com";
const ALICE_AAD: &str = "alice-aad-oid";
const ALICE: &str = "user:alice-aad-oid";
const ALICE_EMAIL: &str = "alice@acme.example";
const BOB_AAD: &str = "bob-aad-oid";
const BOB_EMAIL: &str = "bob@acme.example";
const CONVERSATION_ID: &str = "a:1teams-personal-chat;messageid=1";
const GROUP_CHAT_ID: &str = "19:sales-chat@thread.v2";
const TEAM_ID: &str = "19:team@thread.tacv2";
const CHANNEL_ID: &str = "19:deals@thread.tacv2";
/// The Entra tenant: the one the Graph stand-in serves.
const ENTRA_TENANT: &str = GRAPH_TENANT;
/// The stand-in JWKS publishes the test key twice: endorsed for Teams, and
/// endorsed only for another Bot Framework channel.
const TEAMS_KID: &str = "bf-teams";
const WEBCHAT_KID: &str = "bf-webchat";

/// A `document` skill taking plain text, so an attachment has somewhere to
/// go; each person owns the memos they upload.
const MEMO_SKILL: &str = "---\nkind: skill\nid: memo\ndescription: Text memos.\n\
    owner_field: owner\nacl:\n  create: [owner]\n  read: [owner]\n\
    backend:\n  kind: document\n  accepts: [text/plain]\n---\n# memo\n";
/// A deal only the `sales` group reads: what a directory group must reach.
const DEAL_SKILL: &str = "---\nkind: skill\nid: deal\ndescription: A sales deal.\n\
    acl:\n  read: [sales]\n---\n# deal\n";
const DEAL_PAGE: &str = "markdown/instances/deal/q3.md";
const READ_DEAL: &str = "read: markdown/instances/deal/q3.md";

// ---- Microsoft's side --------------------------------------------------

/// What the stand-in connector saw.
#[derive(Default)]
struct Seen {
    /// `(path, authorization, body)` per activity posted to the connector.
    activities: Vec<(String, String, Value)>,
    /// Form bodies posted to the token endpoint.
    token_requests: Vec<String>,
    /// Record a `message` activity, then drop the connection without an
    /// answer: the reply was delivered, but the bridge cannot know it.
    drop_after_recording: bool,
}

struct BotFramework {
    base: String,
    seen: Arc<Mutex<Seen>>,
    issuer: ExtraIssuer,
}

impl BotFramework {
    async fn start() -> Self {
        let issuer = ExtraIssuer::start().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let metadata = json!({ "issuer": BF_ISSUER, "jwks_uri": format!("{base}/jwks"),
            "id_token_signing_alg_values_supported": ["RS256"] });
        let mut teams_key = issuer.public_jwk(TEAMS_KID);
        teams_key["endorsements"] = json!(["msteams"]);
        let mut webchat_key = issuer.public_jwk(WEBCHAT_KID);
        webchat_key["endorsements"] = json!(["webchat"]);
        let jwks = json!({ "keys": [teams_key, webchat_key] });

        async fn token(State(seen): State<Arc<Mutex<Seen>>>, body: String) -> axum::Json<Value> {
            seen.lock().unwrap().token_requests.push(body);
            axum::Json(json!({ "token_type": "Bearer", "expires_in": 3600,
                               "access_token": "connector-token" }))
        }
        // The raw path: `Path` would percent-decode the conversation id.
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
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let drop_it = {
                let mut seen = seen.lock().unwrap();
                let drop_it = seen.drop_after_recording && body["type"] == "message";
                seen.activities.push((rest, auth, body));
                drop_it
            };
            // A panic ends this connection's task: the client sees the
            // connection close after its request was read, with no response.
            assert!(!drop_it, "stand-in connector: dropping the connection");
            axum::Json(json!({ "id": "sent-1" }))
        }
        // The conversation-members API: who a sender is, beyond the message.
        async fn member(uri: Uri) -> Result<axum::Json<Value>, StatusCode> {
            // The bridge percent-encodes the user id as one path segment.
            let user = uri.path().rsplit('/').next().unwrap_or_default();
            let (aad, email) = match user {
                "29%3Aalice" => (ALICE_AAD, ALICE_EMAIL),
                "29%3Abob" => (BOB_AAD, BOB_EMAIL),
                _ => return Err(StatusCode::NOT_FOUND),
            };
            Ok(axum::Json(
                json!({ "id": user, "aadObjectId": aad, "email": email,
                                  "userPrincipalName": email }),
            ))
        }
        async fn file() -> &'static str {
            "Globex renews in 2028.\n"
        }
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
                get(move || {
                    let k = jwks.clone();
                    async move { axum::Json(k) }
                }),
            )
            .route("/token", post(token))
            .route("/v3/conversations/{*rest}", post(activity).get(member))
            .route("/files/memo.txt", get(file))
            .with_state(Arc::clone(&seen));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { base, seen, issuer }
    }

    /// A channel token as the Bot Framework signs it for this bot.
    fn token(&self, audience: &str) -> String {
        self.token_with_kid(audience, TEAMS_KID)
    }

    fn token_with_kid(&self, audience: &str, kid: &str) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.issuer.sign_with_kid(
            &json!({
                "iss": BF_ISSUER, "aud": audience, "serviceurl": self.base,
                "nbf": now - 5, "iat": now - 5, "exp": now + 600,
            }),
            kid,
        )
    }

    /// The `message` activities sent back (typing excluded), in order.
    fn replies(&self) -> Vec<(String, String, Value)> {
        self.seen
            .lock()
            .unwrap()
            .activities
            .iter()
            .filter(|(_, _, b)| b["type"] == "message")
            .cloned()
            .collect()
    }

    fn activity_types(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .activities
            .iter()
            .map(|(_, _, b)| b["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// `who` @mentions the bot in the group chat [`GROUP_CHAT_ID`].
    fn group_message(&self, id: &str, text: &str, who: (&str, &str)) -> Value {
        let mut m = self.message(id, &format!("<at>escurel</at> {text}"));
        m["from"] = json!({ "id": who.0, "name": "Member", "aadObjectId": who.1 });
        m["conversation"] = json!({ "id": GROUP_CHAT_ID, "conversationType": "groupChat",
                                    "tenantId": ENTRA_TENANT });
        m["entities"] = json!([{ "type": "mention", "text": "<at>escurel</at>",
                                 "mentioned": { "id": format!("28:{APP_ID}"), "name": "escurel" } }]);
        m
    }

    fn message(&self, id: &str, text: &str) -> Value {
        json!({
            "type": "message", "id": id, "channelId": "msteams", "serviceUrl": self.base,
            "timestamp": "2026-11-13T10:00:00.000Z", "text": text,
            "from": { "id": "29:alice", "name": "Alice", "aadObjectId": ALICE_AAD },
            "recipient": { "id": format!("28:{APP_ID}"), "name": "escurel" },
            "conversation": { "id": CONVERSATION_ID, "conversationType": "personal",
                              "tenantId": ENTRA_TENANT },
        })
    }
}

// ---- escurel's side ----------------------------------------------------

/// The bridge's own signing key, for the DEPRECATED self-signed path only:
/// the bridge publishes it under [`BRIDGE_KID`] at `/.well-known/jwks.json`,
/// the JWKS such a gateway trusts as its bridge issuer. Never the gateway's
/// main key (the runner's).
const BRIDGE_KID: &str = "escurel-chat-bridge";

/// How the gateway learns who may do what.
enum Trust<'a> {
    /// The gateway mints every turn (`mint_user_token`) with groups from these directories.
    Minted(escurel_test_support::BridgeMintConfig),
    /// DEPRECATED: the gateway trusts the bridge's own key at this JWKS.
    SelfSigned(&'a ExtraIssuer, String),
}

async fn gateway(trust: Trust<'_>) -> EscurelProcess {
    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::chat_corpus() {
        fixtures = fixtures.page(&page_id, body);
    }
    let fixtures = fixtures
        .skill("memo", MEMO_SKILL)
        .skill("deal", DEAL_SKILL)
        .instance(
            "deal",
            "q3",
            "---\nkind: instance\nskill: deal\nid: q3\n---\n# Q3\nGlobex renews.\n",
        )
        .done();
    let mut overrides = ConfigOverrides {
        write_acl: Some(WriteAclMode::Enforce),
        ..Default::default()
    };
    match trust {
        Trust::Minted(mint) => {
            overrides.signing = true;
            overrides.bridge_mint = Some(mint);
        }
        Trust::SelfSigned(key, jwks) => {
            overrides.extra_issuers = vec![(key.issuer_url().to_owned(), jwks)];
            overrides.bridge_issuer = Some(key.issuer_url().to_owned());
            overrides.bridge_kids = vec![BRIDGE_KID.to_owned()];
        }
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: overrides,
        fixtures: Some(fixtures),
    })
    .await
}

fn runner(gw: &EscurelProcess, sleep_ms: u64) -> RunningRunner {
    RunnerProcess::echo(gw, TENANT)
        .minted()
        .env("ESCUREL_ECHO_SLEEP_MS", sleep_ms.to_string())
        .spawn()
}

/// The bridge binary, restartable on the same state file.
struct Bridge {
    child: Option<Child>,
    listen: String,
    env: Vec<(String, String)>,
    /// Where the bridge's stdout (its JSON log) goes, when a test reads it.
    log: Option<std::path::PathBuf>,
    _state: tempfile::TempDir,
}

impl Bridge {
    /// The bridge on `listen` (kept across restarts: its JWKS URL is in the
    /// gateway's config), talking to the gateway at `gateway_url`.
    fn spawn(
        listen: String,
        gateway_url: &str,
        auth_env: Vec<(&'static str, String)>,
        bf: &BotFramework,
        extra_env: &[(&str, &str)],
    ) -> Self {
        let state = tempfile::tempdir().unwrap();
        let env = [
            ("ESCUREL_BRIDGE_GATEWAY_URL", gateway_url.to_owned()),
            ("ESCUREL_BRIDGE_TENANT", TENANT.to_owned()),
            (
                "ESCUREL_BRIDGE_STATE_PATH",
                state.path().join("bridge.duckdb").display().to_string(),
            ),
            ("ESCUREL_BRIDGE_TYPING_INTERVAL", "200ms".to_owned()),
            ("ESCUREL_BRIDGE_POLL_INTERVAL", "100ms".to_owned()),
            ("ESCUREL_BRIDGE_TEAMS_APP_ID", APP_ID.to_owned()),
            (
                "ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS",
                format!("some-other-tenant,{ENTRA_TENANT}"),
            ),
            // Allow-listed, so the attachment test proves the address check
            // refuses it, not the list.
            (
                "ESCUREL_BRIDGE_TEAMS_ATTACHMENT_HOSTS",
                "*.sharepoint.com,localhost".to_owned(),
            ),
            (
                "ESCUREL_BRIDGE_TEAMS_APP_PASSWORD",
                "not-a-real-secret".to_owned(),
            ),
            (
                "ESCUREL_BRIDGE_TEAMS_TOKEN_URL",
                format!("{}/token", bf.base),
            ),
            (
                "ESCUREL_BRIDGE_TEAMS_OPENID_METADATA_URL",
                format!("{}/openid", bf.base),
            ),
        ]
        .into_iter()
        .chain(auth_env)
        .map(|(k, v)| (k.to_owned(), v))
        .chain(
            extra_env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        )
        .collect();
        let mut bridge = Self {
            child: None,
            listen,
            env,
            log: None,
            _state: state,
        };
        bridge.start();
        bridge
    }

    fn start(&mut self) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-chat-bridge"));
        cmd.env("ESCUREL_BRIDGE_LISTEN", &self.listen);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if let Some(log) = &self.log {
            cmd.stdout(std::fs::File::create(log).expect("bridge log"));
        }
        self.child = Some(cmd.spawn().expect("spawn escurel-chat-bridge"));
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn restart(&mut self) {
        self.kill();
        self.start();
    }

    async fn wait_healthy(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.as_mut().and_then(|c| c.try_wait().unwrap()) {
                panic!("bridge exited before it became healthy: {status:?}");
            }
            if let Ok(r) = reqwest::get(format!("http://{}/healthz", self.listen)).await
                && r.status().is_success()
            {
                return;
            }
            assert!(Instant::now() < deadline, "bridge never became healthy");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// POST an activity to the messaging endpoint; the HTTP status.
    async fn post(&self, token: Option<&str>, activity: &Value) -> StatusCode {
        let mut req = reqwest::Client::new()
            .post(format!("http://{}/api/messages", self.listen))
            .json(activity);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let status = req.send().await.expect("post activity").status();
        StatusCode::from_u16(status.as_u16()).unwrap()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.kill();
    }
}

struct World {
    gw: EscurelProcess,
    bf: BotFramework,
    _runner: Option<RunningRunner>,
    bridge: Bridge,
    _bridge_key: ExtraIssuer,
    /// The group-directory stand-ins (whichever the setup lists are configured).
    dirs: Dirs,
    /// Between the bridge and the gateway; only the bridge's link goes down.
    link: Link,
}

struct Dirs {
    graph: DirectoryStandIn,
    google: DirectoryStandIn,
    okta: DirectoryStandIn,
}

/// How a test's world differs from the plain one.
#[derive(Default)]
struct Setup {
    /// How long each run idles; `None` starts no runner at all.
    run_ms: Option<u64>,
    /// Extra `ESCUREL_BRIDGE_*` settings.
    bridge_env: Vec<(&'static str, &'static str)>,
    /// The group directories the gateway asks; empty = Graph alone.
    directories: Vec<&'static str>,
    /// The membership cache's fresh / stale windows; `None` = production.
    cache: Option<(Duration, Duration)>,
    /// The DEPRECATED self-signed bridge key instead of the gateway's mint.
    self_signed: bool,
    /// Keep the bridge's log for the test to read.
    keep_log: bool,
}

async fn world(run_ms: u64) -> World {
    world_with(Setup {
        run_ms: Some(run_ms),
        ..Setup::default()
    })
    .await
}

async fn world_with(setup: Setup) -> World {
    let bridge_key = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let listen = format!("127.0.0.1:{}", free_port());
    let dirs = Dirs {
        graph: DirectoryStandIn::graph().await,
        google: DirectoryStandIn::google().await,
        okta: DirectoryStandIn::okta().await,
    };
    // Alice is in `escurel-sales` by Graph unless a test says otherwise.
    dirs.graph.member_of(ALICE_AAD, &["escurel-sales"]);
    dirs.graph.member_of(BOB_AAD, &["escurel-support"]);
    let gw = if setup.self_signed {
        gateway(Trust::SelfSigned(
            &bridge_key,
            format!("http://{listen}/.well-known/jwks.json"),
        ))
        .await
    } else {
        let listed = if setup.directories.is_empty() {
            vec!["graph"]
        } else {
            setup.directories.clone()
        };
        let configs = listed
            .iter()
            .map(|d| match *d {
                "graph" => dirs.graph.config("escurel-"),
                "google" => dirs.google.config("escurel-"),
                _ => dirs.okta.config("escurel-"),
            })
            .collect();
        gateway(Trust::Minted(bridge_mint(
            "user:{aadObjectId}",
            configs,
            setup.cache,
        )))
        .await
    };
    let bf = BotFramework::start().await;
    let runner = setup.run_ms.map(|ms| runner(&gw, ms));
    let link = Link::start(gw.base_url()).await;
    let auth_env = if setup.self_signed {
        let (key, kid) = bridge_key.signing_material();
        vec![
            (
                "ESCUREL_BRIDGE_AUTH_ISSUER",
                bridge_key.issuer_url().to_owned(),
            ),
            ("ESCUREL_BRIDGE_AUTH_KID", kid.to_owned()),
            ("ESCUREL_BRIDGE_AUTH_SIGNING_KEY", key),
            (
                "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE",
                "user:{aadObjectId}".to_owned(),
            ),
        ]
    } else {
        vec![(
            "ESCUREL_BRIDGE_GATEWAY_TOKEN",
            gw.mint_token_with_groups(TENANT, "svc:chat-bridge", &["escurel:bridge"], false),
        )]
    };
    let mut bridge = Bridge::spawn(listen, &link.url, auth_env, &bf, &setup.bridge_env);
    if setup.keep_log {
        bridge.log = Some(bridge._state.path().join("bridge.log"));
        bridge.restart();
    }
    bridge.wait_healthy().await;
    World {
        gw,
        bf,
        _runner: runner,
        bridge,
        _bridge_key: bridge_key,
        dirs,
        link,
    }
}

/// A TCP link from the bridge to the gateway that a test can cut and
/// restore: cut, it closes every open connection and refuses new ones — the
/// bridge sees the gateway gone, the way a stop-first deploy looks to it.
struct Link {
    url: String,
    up: tokio::sync::watch::Sender<bool>,
}

impl Link {
    async fn start(target: &str) -> Self {
        let target = target
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (up, watch) = tokio::sync::watch::channel(true);
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if !*watch.borrow() {
                    continue;
                }
                let (target, mut watch) = (target.clone(), watch.clone());
                tokio::spawn(async move {
                    let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await else {
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = watch.wait_for(|up| !*up) => {}
                    }
                });
            }
        });
        Self { url, up }
    }

    fn cut(&self) {
        self.up.send_replace(false);
    }

    fn restore(&self) {
        self.up.send_replace(true);
    }
}

async fn call(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// The `chat` events captured so far.
async fn chat_events(gw: &EscurelProcess) -> Vec<Value> {
    let admin = gw.mint_token(TENANT, Role::Admin);
    call(gw, &admin, "list_events", json!({ "label_skill": "chat" })).await["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn wait_for_replies(
    bf: &BotFramework,
    n: usize,
    within: Duration,
) -> Vec<(String, String, Value)> {
    let deadline = Instant::now() + within;
    loop {
        let replies = bf.replies();
        if replies.len() >= n {
            return replies;
        }
        assert!(
            Instant::now() < deadline,
            "{n} replies never arrived; activities: {:?}",
            bf.activity_types()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The asker's Teams conversation page and its chat history, oldest first.
async fn alice_history(gw: &EscurelProcess) -> (String, Vec<Value>) {
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let listed = call(
        gw,
        &alice,
        "list_instances",
        json!({ "skill": "conversation" }),
    )
    .await;
    let instances = listed["instances"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        instances.len(),
        1,
        "one conversation page per Teams chat: {listed}"
    );
    let page_id = instances[0]["page_id"].as_str().unwrap().to_owned();
    use sha2::Digest as _;
    let group = format!(
        "page-chat:v1:{:x}:{page_id}",
        sha2::Sha256::digest(ALICE.as_bytes())
    );
    let msgs = call(
        gw,
        &alice,
        "list_messages",
        json!({ "chat_group_id": group, "direction": "asc" }),
    )
    .await["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (page_id, msgs)
}

// ---- the tests ---------------------------------------------------------

#[tokio::test]
async fn a_teams_message_gets_exactly_one_reply_and_its_history_lands_under_the_chat_group() {
    // The run idles a moment so the bridge has a live run to show typing for.
    let w = world(1_500).await;
    let token = w.bf.token(APP_ID);
    let status = w
        .bridge
        .post(
            Some(&token),
            &w.bf.message("act-1", "Which customers renew?"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    let (path, auth, body) = &replies[0];
    assert_eq!(body["text"], "echo: Which customers renew?", "{body}");
    assert_eq!(body["replyToId"], "act-1");
    assert_eq!(body["conversation"]["id"], CONVERSATION_ID);
    assert_eq!(body["recipient"]["id"], "29:alice");
    assert_eq!(
        path,
        &format!("{}/activities/act-1", urlencode(CONVERSATION_ID)),
        "a reply to the activity, on its conversation"
    );
    assert_eq!(auth, "Bearer connector-token", "client-credentials bearer");
    let form = w.bf.seen.lock().unwrap().token_requests[0].clone();
    assert!(form.contains("grant_type=client_credentials"), "{form}");
    assert!(form.contains(&format!("client_id={APP_ID}")), "{form}");

    // Typing was shown while the run was live, before the answer.
    let types = w.bf.activity_types();
    let first_message = types.iter().position(|t| t == "message").unwrap();
    assert!(
        types[..first_message].iter().any(|t| t == "typing"),
        "a typing activity while the run is live: {types:?}"
    );

    // The turn and its answer are the asker's page-chat history.
    let (_, msgs) = alice_history(&w.gw).await;
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "Which customers renew?");
    assert_eq!(msgs[0]["author"], ALICE, "captured as the asker");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"], "echo: Which customers renew?");
    let events = chat_events(&w.gw).await;
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(events[0]["provenance"]["captured_by"], ALICE);
    assert_eq!(
        events[0]["provenance"]["captured_via"], "bridge:msteams",
        "the gateway stamps that the bridge relayed it"
    );

    // Nothing else follows.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 1);
}

#[tokio::test]
async fn a_bad_signature_or_a_foreign_audience_is_refused_and_nothing_is_captured() {
    let w = world(0).await;
    let activity = w.bf.message("act-x", "Which customers renew?");

    let good = w.bf.token(APP_ID);
    let (head, sig) = good.rsplit_once('.').unwrap();
    // Flip the signature's first character: same claims, broken signature.
    let flipped = if sig.starts_with('A') { "B" } else { "A" };
    let forged = format!("{head}.{flipped}{}", &sig[1..]);
    assert_eq!(
        w.bridge.post(Some(&forged), &activity).await,
        StatusCode::UNAUTHORIZED
    );
    let foreign = w.bf.token("some-other-bot");
    assert_eq!(
        w.bridge.post(Some(&foreign), &activity).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.bridge.post(None, &activity).await,
        StatusCode::UNAUTHORIZED
    );
    // A genuine token whose `serviceurl` is not the activity's: a replay to
    // another connector.
    let mut elsewhere = activity.clone();
    elsewhere["serviceUrl"] = json!("http://127.0.0.1:9/");
    assert_eq!(
        w.bridge.post(Some(&good), &elsewhere).await,
        StatusCode::UNAUTHORIZED
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        chat_events(&w.gw).await.is_empty(),
        "no capture on a refused activity"
    );
    assert!(w.bf.activity_types().is_empty(), "nothing sent back");
}

#[tokio::test]
async fn only_a_teams_personal_chat_from_an_allowed_tenant_is_answered() {
    let w = world(0).await;
    let token = w.bf.token(APP_ID);

    // Another Bot Framework channel this bot is enabled on, on a key the
    // JWKS endorses for Teams.
    let mut webchat = w.bf.message("act-c", "Which customers renew?");
    webchat["channelId"] = json!("webchat");
    // A genuine Teams activity, signed with a key endorsed only for webchat.
    let unendorsed = w.bf.token_with_kid(APP_ID, WEBCHAT_KID);
    // A person in an Entra tenant the deployment does not serve, by either
    // field Teams names it with.
    let mut foreign = w.bf.message("act-t", "Which customers renew?");
    foreign["conversation"]["tenantId"] = json!("foreign-tenant");
    let mut foreign_channel_data = w.bf.message("act-t2", "Which customers renew?");
    foreign_channel_data["conversation"]
        .as_object_mut()
        .unwrap()
        .remove("tenantId");
    foreign_channel_data["channelData"] = json!({ "tenant": { "id": "foreign-tenant" } });
    // A conversation that does not say it is 1:1.
    let mut untyped = w.bf.message("act-u", "Which customers renew?");
    untyped["conversation"]
        .as_object_mut()
        .unwrap()
        .remove("conversationType");
    let mut group = w.bf.message("act-g", "Which customers renew?");
    group["conversation"]["conversationType"] = json!("groupChat");

    w.bridge.post(Some(&token), &webchat).await;
    w.bridge
        .post(
            Some(&unendorsed),
            &w.bf.message("act-e", "Which customers renew?"),
        )
        .await;
    for activity in [&foreign, &foreign_channel_data, &untyped, &group] {
        w.bridge.post(Some(&token), activity).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        chat_events(&w.gw).await.is_empty(),
        "none of them is captured: {:#?}",
        chat_events(&w.gw).await
    );
    assert!(w.bf.activity_types().is_empty(), "nothing sent back");

    // The same world answers a proper one: the refusals are the reason.
    let mut allowed = w.bf.message("act-ok", "Which customers renew?");
    allowed["conversation"]
        .as_object_mut()
        .unwrap()
        .remove("tenantId");
    allowed["channelData"] = json!({ "tenant": { "id": ENTRA_TENANT } });
    assert_eq!(w.bridge.post(Some(&token), &allowed).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(chat_events(&w.gw).await.len(), 1);
}

#[tokio::test]
async fn a_replayed_activity_starts_no_second_run_and_gets_no_second_reply() {
    let w = world(0).await;
    let token = w.bf.token(APP_ID);
    let activity = w.bf.message("act-2", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    // The channel retries before the answer is out, and again after.
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 1, "one reply per activity");
    assert_eq!(chat_events(&w.gw).await.len(), 1, "one run per activity");
    assert_eq!(alice_history(&w.gw).await.1.len(), 2);
}

#[tokio::test]
async fn a_bridge_restart_mid_run_still_sends_exactly_one_reply() {
    // Long enough that the bridge is killed while the run is in flight.
    let mut w = world(4_000).await;
    let token = w.bf.token(APP_ID);
    let activity = w.bf.message("act-3", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    let deadline = Instant::now() + Duration::from_secs(20);
    while chat_events(&w.gw).await.is_empty() {
        assert!(Instant::now() < deadline, "the turn was never captured");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    w.bridge.restart();
    w.bridge.wait_healthy().await;

    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(replies[0].2["text"], "echo: Which customers renew?");
    // A second restart after the reply sends nothing again.
    w.bridge.restart();
    w.bridge.wait_healthy().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 1, "{:?}", w.bf.activity_types());
    assert_eq!(chat_events(&w.gw).await.len(), 1);
}

#[tokio::test]
async fn an_attachment_is_ingested_when_the_gateway_takes_it_and_listed_when_not() {
    let w = world(0).await;
    let token = w.bf.token(APP_ID);
    let mut activity = w.bf.message("act-4", "What does the memo say?");
    activity["attachments"] = json!([
        { "contentType": "text/plain", "name": "memo.txt",
          "contentUrl": format!("{}/files/memo.txt", w.bf.base) },
        { "contentType": "image/png", "name": "photo.png",
          "contentUrl": format!("{}/files/photo.png", w.bf.base) },
    ]);
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;

    let (_, msgs) = alice_history(&w.gw).await;
    let turn = msgs[0]["content"].as_str().unwrap();
    let alice = w.gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let memos = call(&w.gw, &alice, "list_instances", json!({ "skill": "memo" })).await;
    let memo_page = memos["instances"][0]["page_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the text attachment became a memo: {memos}; turn: {turn}"))
        .to_owned();
    assert!(turn.starts_with("What does the memo say?"), "{turn}");
    assert!(
        turn.contains(&memo_page),
        "the ingested page is named: {turn}"
    );
    assert!(
        turn.contains("photo.png") && turn.contains("not ingested"),
        "the refused attachment is listed with why: {turn}"
    );
}

#[tokio::test]
async fn an_attachment_on_an_internal_address_is_refused_and_never_fetched() {
    let w = world(0).await;
    // A reachable internal service: it would answer, so a refusal is the
    // bridge's decision, not a dead port.
    let hits = Arc::new(Mutex::new(0usize));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = listener.local_addr().unwrap().port();
    let counter = Arc::clone(&hits);
    let app = Router::new().route(
        "/{*rest}",
        get(move || {
            let c = Arc::clone(&counter);
            async move {
                *c.lock().unwrap() += 1;
                "internal secret\n"
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let token = w.bf.token(APP_ID);
    let mut activity = w.bf.message("act-5", "What does the memo say?");
    activity["attachments"] = json!([
        // Plain http to a loopback address that is not the connector.
        { "contentType": "text/plain", "name": "metrics.txt",
          "contentUrl": format!("http://127.0.0.1:{internal}/metrics") },
        // https, on an allow-listed host, that resolves to loopback.
        { "contentType": "text/plain", "name": "local.txt",
          "contentUrl": format!("https://localhost:{internal}/x") },
        // A Teams file link pointing at the same internal service.
        { "contentType": "application/vnd.microsoft.teams.file.download.info",
          "name": "file.txt",
          "content": { "downloadUrl": format!("http://127.0.0.1:{internal}/file") } },
    ]);
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;

    let (_, msgs) = alice_history(&w.gw).await;
    let turn = msgs[0]["content"].as_str().unwrap().to_owned();
    for name in ["metrics.txt", "local.txt", "file.txt"] {
        assert!(
            turn.lines()
                .any(|l| l.contains(name) && l.contains("not ingested")),
            "{name} is listed as refused: {turn}"
        );
    }
    let alice = w.gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let memos = call(&w.gw, &alice, "list_instances", json!({ "skill": "memo" })).await;
    assert_eq!(
        memos["instances"].as_array().map_or(0, Vec::len),
        0,
        "nothing internal is ingested: {memos}"
    );
    assert_eq!(
        *hits.lock().unwrap(),
        0,
        "the internal service was never asked"
    );
}

#[tokio::test]
async fn a_reply_that_may_have_been_delivered_is_not_sent_again() {
    let w = world(0).await;
    w.bf.seen.lock().unwrap().drop_after_recording = true;
    let token = w.bf.token(APP_ID);
    let activity = w.bf.message("act-6", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    // The old loop retried after 1 s and 2 s.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        w.bf.replies().len(),
        1,
        "a send that reached the connector is never repeated: {:?}",
        w.bf.activity_types()
    );
}

/// The bridge's timeout notice.
const TIMED_OUT: &str = "I couldn't get an answer in time. Please ask again.";

/// A gateway outage longer than the old four tries (crew U-F2): the bridge
/// keeps retrying the turn with capped backoff, so once the gateway is back
/// the person gets exactly one answer — and the bridge was never restarted.
#[tokio::test]
async fn a_gateway_outage_mid_turn_still_ends_in_exactly_one_reply_without_a_bridge_restart() {
    let w = world(0).await;
    let token = w.bf.token(APP_ID);
    w.link.cut();
    let activity = w.bf.message("act-7", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &activity).await, StatusCode::OK);
    // The old loop gave up after four tries, 2+4+8 s apart.
    tokio::time::sleep(Duration::from_secs(16)).await;
    assert!(chat_events(&w.gw).await.is_empty(), "nothing got through");
    w.link.restore();

    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(replies[0].2["text"], "echo: Which customers renew?");
    assert_eq!(chat_events(&w.gw).await.len(), 1, "one run");
    // A later turn is answered too, and the first is not answered again.
    let later = w.bf.message("act-8", "And who churns?");
    assert_eq!(w.bridge.post(Some(&token), &later).await, StatusCode::OK);
    let replies = wait_for_replies(&w.bf, 2, Duration::from_secs(60)).await;
    assert_eq!(replies.len(), 2, "{:?}", w.bf.activity_types());
    assert_eq!(replies[1].2["replyToId"], "act-8");
}

/// No runner, a short reply timeout (crew U-F9): the person is told once that
/// no answer came, and a restart does not tell them again.
#[tokio::test]
async fn with_no_runner_the_timeout_notice_is_sent_exactly_once() {
    let mut w = world_with(Setup {
        run_ms: None,
        bridge_env: vec![("ESCUREL_BRIDGE_REPLY_TIMEOUT", "2s")],
        ..Setup::default()
    })
    .await;
    let token = w.bf.token(APP_ID);
    let first = w.bf.message("act-9", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &first).await, StatusCode::OK);
    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(30)).await;
    assert_eq!(replies[0].2["text"], TIMED_OUT);
    assert_eq!(replies[0].2["replyToId"], "act-9");

    // After a restart (which resumes open turns), a second turn times out
    // in its turn: the bridge had its chance at the first, and left it.
    w.bridge.restart();
    w.bridge.wait_healthy().await;
    let second = w.bf.message("act-10", "Anyone there?");
    assert_eq!(w.bridge.post(Some(&token), &second).await, StatusCode::OK);
    let replies = wait_for_replies(&w.bf, 2, Duration::from_secs(30)).await;
    let to: Vec<_> = replies.iter().map(|r| r.2["replyToId"].clone()).collect();
    assert_eq!(to, vec![json!("act-9"), json!("act-10")], "one notice each");
}

/// A sender the principal template cannot map (no `aadObjectId`, crew U-F9)
/// gets nothing — no capture, no reply, no retry after a restart — while the
/// same bridge answers a mapped person before and after.
#[tokio::test]
async fn an_unmapped_sender_gets_no_event_and_no_activity_even_after_a_restart() {
    let mut w = world(0).await;
    let token = w.bf.token(APP_ID);
    let mut stranger = w.bf.message("act-11", "Who am I?");
    stranger["from"]
        .as_object_mut()
        .unwrap()
        .remove("aadObjectId");
    assert_eq!(w.bridge.post(Some(&token), &stranger).await, StatusCode::OK);
    // A mapped person's turn after it: once that is answered, the stranger's
    // earlier turn has been worked.
    let known = w.bf.message("act-12", "Which customers renew?");
    assert_eq!(w.bridge.post(Some(&token), &known).await, StatusCode::OK);
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;

    w.bridge.restart();
    w.bridge.wait_healthy().await;
    let again = w.bf.message("act-13", "And who churns?");
    assert_eq!(w.bridge.post(Some(&token), &again).await, StatusCode::OK);
    let replies = wait_for_replies(&w.bf, 2, Duration::from_secs(60)).await;
    let to: Vec<_> = replies.iter().map(|r| r.2["replyToId"].clone()).collect();
    assert_eq!(to, vec![json!("act-12"), json!("act-13")]);
    let stranger_activities =
        w.bf.seen
            .lock()
            .unwrap()
            .activities
            .iter()
            .filter(|(path, _, body)| path.contains("act-11") || body["replyToId"] == "act-11")
            .count();
    assert_eq!(
        stranger_activities, 0,
        "no activity of any kind for the stranger"
    );
    let events = chat_events(&w.gw).await;
    assert_eq!(events.len(), 2, "{events:#?}");
    assert!(
        events
            .iter()
            .all(|e| e["provenance"]["channel"]["message_id"] != "act-11"),
        "{events:#?}"
    );
}

/// Percent-encode a path segment the way the bridge does (RFC 3986 unreserved kept).
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---- #830: gateway-minted turns, directory groups, group chats ------------

/// The reply to `READ_DEAL` when the run's subject `who` reads the deal (`ok`) or not.
fn read_reply(who: &str, outcome: &str) -> String {
    format!("echo: {READ_DEAL}\n\nas {who}: read {DEAL_PAGE} {outcome}")
}

async fn chat_events_with(gw: &EscurelProcess, n: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = chat_events(gw).await;
        if events.len() >= n {
            return events;
        }
        assert!(Instant::now() < deadline, "{n} chat events never arrived");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A 1:1 asker in `escurel-sales` reads the `read: [sales]` deal, whichever
/// directory says so — Graph by object id, Google and Okta by the email the
/// members API names — and through a union of two (this inverts
/// `bridge_token.rs`'s "a bridge token carries no groups").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_to_one_asker_reads_a_group_page_via_each_directory_and_a_union() {
    for (directories, graph, google, okta, groups) in [
        (
            vec!["graph"],
            &["escurel-sales"][..],
            &[][..],
            &[][..],
            json!(["sales"]),
        ),
        (
            vec!["google"],
            &[][..],
            &["escurel-sales"][..],
            &[][..],
            json!(["sales"]),
        ),
        (
            vec!["okta"],
            &[][..],
            &[][..],
            &["escurel-sales"][..],
            json!(["sales"]),
        ),
        (
            vec!["google", "okta"],
            &[][..],
            &["escurel-ops"][..],
            &["escurel-sales"][..],
            json!(["ops", "sales"]),
        ),
    ] {
        let what = directories.join("+");
        let w = world_with(Setup {
            run_ms: Some(0),
            directories,
            ..Setup::default()
        })
        .await;
        w.dirs.graph.member_of(ALICE_AAD, graph);
        w.dirs.google.member_of(ALICE_EMAIL, google);
        w.dirs.okta.member_of(ALICE_EMAIL, okta);
        let token = w.bf.token(APP_ID);
        assert_eq!(
            w.bridge
                .post(Some(&token), &w.bf.message("act-d1", READ_DEAL))
                .await,
            StatusCode::OK
        );
        let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
        assert_eq!(replies[0].2["text"], read_reply(ALICE, "ok"), "{what}");
        let events = chat_events(&w.gw).await;
        assert_eq!(events[0]["provenance"]["captured_by"], ALICE, "{what}");
        assert_eq!(events[0]["provenance"]["captured_groups"], groups, "{what}");
    }
}

/// CONTROL for the above: the same asker with no directory group is refused
/// the deal by the run's own token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_to_one_asker_outside_the_group_does_not_read_its_page() {
    let w = world(0).await;
    w.dirs.graph.member_of(ALICE_AAD, &["escurel-support"]);
    let token = w.bf.token(APP_ID);
    w.bridge
        .post(Some(&token), &w.bf.message("act-d2", READ_DEAL))
        .await;
    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(replies[0].2["text"], read_reply(ALICE, "refused"));
}

async fn map_chat(gw: &EscurelProcess, id: &str, target: &str, group: &str) {
    let admin = gw.mint_token(TENANT, Role::Admin);
    let wrote = call(
        gw,
        &admin,
        "update_page",
        json!({ "page_id": format!("markdown/instances/chat-channel/{id}.md"),
                "content": format!("---\nkind: instance\nskill: chat-channel\nid: {id}\n\
                    channel: msteams\n{target}\ngroup: {group}\n---\n# {id}\n") }),
    )
    .await;
    assert_eq!(wrote["ok"], true, "{wrote}");
}

/// A group chat mapped to `sales`: a member's @mention is answered once, in
/// the conversation, by a run acting as `group:sales`, with the asker
/// recorded — and the membership comes from Okta alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mapped_group_chat_is_answered_once_as_its_group_for_a_member() {
    let w = world_with(Setup {
        run_ms: Some(0),
        directories: vec!["graph", "okta"],
        ..Setup::default()
    })
    .await;
    w.dirs.graph.member_of(ALICE_AAD, &["escurel-support"]);
    w.dirs.okta.member_of(ALICE_EMAIL, &["escurel-sales"]);
    map_chat(
        &w.gw,
        "sales-chat",
        &format!("conversation_id: \"{GROUP_CHAT_ID}\""),
        "sales",
    )
    .await;
    let token = w.bf.token(APP_ID);
    // Without the @mention it is just chatter in the room.
    let mut chatter =
        w.bf.group_message("act-g0", READ_DEAL, ("29:alice", ALICE_AAD));
    chatter.as_object_mut().unwrap().remove("entities");
    w.bridge.post(Some(&token), &chatter).await;
    let asked =
        w.bf.group_message("act-g1", READ_DEAL, ("29:alice", ALICE_AAD));
    assert_eq!(w.bridge.post(Some(&token), &asked).await, StatusCode::OK);

    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    let (path, _, body) = &replies[0];
    assert_eq!(body["text"], read_reply("group:sales", "ok"), "{body}");
    assert_eq!(
        path,
        &format!("{}/activities", urlencode(GROUP_CHAT_ID)),
        "a group-chat answer is an activity in the conversation"
    );
    let events = chat_events(&w.gw).await;
    assert_eq!(
        events.len(),
        1,
        "one run, the mention stripped: {events:#?}"
    );
    assert_eq!(events[0]["provenance"]["captured_by"], "group:sales");
    assert_eq!(events[0]["provenance"]["asked_by"], ALICE);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 1, "exactly one reply");
}

/// An unmapped group chat: one polite refusal naming how to map it, no run.
/// A stranger in a mapped chat: one refusal, no run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unmapped_chat_or_a_non_member_gets_one_refusal_and_no_run() {
    let w = world(0).await;
    let token = w.bf.token(APP_ID);
    let first =
        w.bf.group_message("act-u1", "who renews?", ("29:alice", ALICE_AAD));
    w.bridge.post(Some(&token), &first).await;
    // The channel retries: still one refusal.
    w.bridge.post(Some(&token), &first).await;
    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    let text = replies[0].2["text"].as_str().unwrap();
    assert!(
        text.contains("isn't connected")
            && text.contains(GROUP_CHAT_ID)
            && text.contains("chat-channel"),
        "{text}"
    );

    map_chat(
        &w.gw,
        "sales-chat",
        &format!("conversation_id: \"{GROUP_CHAT_ID}\""),
        "sales",
    )
    .await;
    let stranger = w.bf.group_message("act-u2", READ_DEAL, ("29:bob", BOB_AAD));
    w.bridge.post(Some(&token), &stranger).await;
    let replies = wait_for_replies(&w.bf, 2, Duration::from_secs(60)).await;
    assert!(
        replies[1].2["text"]
            .as_str()
            .unwrap()
            .contains("not a member"),
        "{:?}",
        replies[1].2
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 2, "one refusal each");
    assert!(chat_events(&w.gw).await.is_empty(), "no run for either");
}

/// A channel thread: mapped by team and channel, answered on the thread's root.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channel_thread_is_answered_on_its_root() {
    let w = world(0).await;
    map_chat(
        &w.gw,
        "deals",
        &format!("team_id: \"{TEAM_ID}\"\nchannel_id: \"{CHANNEL_ID}\""),
        "sales",
    )
    .await;
    let thread = format!("{CHANNEL_ID};messageid=root-1");
    let mut m =
        w.bf.group_message("act-c5", READ_DEAL, ("29:alice", ALICE_AAD));
    m["conversation"] = json!({ "id": thread, "conversationType": "channel",
                                "tenantId": ENTRA_TENANT });
    m["channelData"] = json!({ "team": { "id": TEAM_ID }, "channel": { "id": CHANNEL_ID },
                               "tenant": { "id": ENTRA_TENANT } });
    let token = w.bf.token(APP_ID);
    assert_eq!(w.bridge.post(Some(&token), &m).await, StatusCode::OK);
    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    let (path, _, body) = &replies[0];
    assert_eq!(body["text"], read_reply("group:sales", "ok"));
    assert_eq!(body["replyToId"], "root-1");
    assert_eq!(path, &format!("{}/activities/root-1", urlencode(&thread)));
}

/// The directory down: inside the stale window the cached groups still
/// answer; past it the person is told once that access cannot be checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_directory_outage_uses_the_cache_then_tells_the_person_once() {
    let w = world_with(Setup {
        run_ms: Some(0),
        cache: Some((Duration::from_millis(200), Duration::from_secs(4))),
        ..Setup::default()
    })
    .await;
    let token = w.bf.token(APP_ID);
    w.bridge
        .post(Some(&token), &w.bf.message("act-o1", READ_DEAL))
        .await;
    wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    w.dirs.graph.down();
    w.bridge
        .post(Some(&token), &w.bf.message("act-o2", READ_DEAL))
        .await;
    let replies = wait_for_replies(&w.bf, 2, Duration::from_secs(60)).await;
    assert_eq!(
        replies[1].2["text"],
        read_reply(ALICE, "ok"),
        "the cached groups stand in"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    w.bridge
        .post(Some(&token), &w.bf.message("act-o3", READ_DEAL))
        .await;
    let replies = wait_for_replies(&w.bf, 3, Duration::from_secs(60)).await;
    assert_eq!(
        replies[2].2["text"],
        "I can't check your access right now. Please try again in a few minutes."
    );
    assert_eq!(replies[2].2["replyToId"], "act-o3");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.bf.replies().len(), 3);
    assert_eq!(
        chat_events_with(&w.gw, 2).await.len(),
        2,
        "no run for the third"
    );
}

/// The DEPRECATED self-signed path still answers a 1:1 chat for this
/// release — with no groups — and says it is deprecated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_self_signed_bridge_key_still_works_and_logs_its_deprecation() {
    let w = world_with(Setup {
        run_ms: Some(0),
        self_signed: true,
        keep_log: true,
        ..Setup::default()
    })
    .await;
    let token = w.bf.token(APP_ID);
    w.bridge
        .post(Some(&token), &w.bf.message("act-l1", READ_DEAL))
        .await;
    let replies = wait_for_replies(&w.bf, 1, Duration::from_secs(60)).await;
    assert_eq!(
        replies[0].2["text"],
        read_reply(ALICE, "refused"),
        "a self-signed turn carries no groups"
    );
    let log = std::fs::read_to_string(w.bridge.log.as_ref().unwrap()).unwrap();
    assert!(
        log.lines()
            .any(|l| l.contains("\"level\":\"warn\"") && l.contains("DEPRECATED")),
        "{log}"
    );
}
