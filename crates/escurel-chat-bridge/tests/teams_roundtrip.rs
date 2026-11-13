//! The Teams chat bridge end to end (#824): a Bot Framework message activity
//! in, exactly one reply out, the turn's history under the person's chat group.
//!
//! escurel is real throughout: the gateway (`EscurelProcess`, write ACL
//! enforced, trusting the bridge's own key as a restricted bridge kid), the
//! runner binary in minted mode on the echo harness (it answers a chat turn
//! with `echo: <the question>`), and the `escurel-chat-bridge` binary. Only Microsoft's side is a stand-in, served from this file: the
//! OpenID metadata + JWKS the channel token is verified against (signed with a
//! test RSA key), the client-credentials token endpoint, and the connector that
//! records every activity the bridge sends back.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::{get, post};
use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, ExtraIssuer, FixtureBuilder, Opts, Role,
    RunnerProcess, RunningRunner, WriteAclMode, free_port,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const APP_ID: &str = "test-bot-app";
const BF_ISSUER: &str = "https://api.botframework.com";
const ALICE_AAD: &str = "alice-aad-oid";
const ALICE: &str = "user:alice-aad-oid";
const CONVERSATION_ID: &str = "a:1teams-personal-chat;messageid=1";
const ENTRA_TENANT: &str = "entra-tenant";
/// The stand-in JWKS publishes the test key twice: endorsed for Teams, and
/// endorsed only for another Bot Framework channel.
const TEAMS_KID: &str = "bf-teams";
const WEBCHAT_KID: &str = "bf-webchat";

/// A `document` skill taking plain text, so an attachment has somewhere to
/// go; each person owns the memos they upload.
const MEMO_SKILL: &str = "---\nkind: skill\nid: memo\ndescription: Text memos.\n\
    owner_field: owner\nacl:\n  create: [owner]\n  read: [owner]\n\
    backend:\n  kind: document\n  accepts: [text/plain]\n---\n# memo\n";

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
            .route("/v3/conversations/{*rest}", post(activity))
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

/// The bridge's own signing key: a JWKS the gateway trusts publishes it under
/// [`BRIDGE_KID`], which the gateway lists as a bridge kid. It is never the
/// gateway's main key (the runner's).
const BRIDGE_KID: &str = "escurel-chat-bridge";

async fn gateway(bridge_key: &ExtraIssuer) -> EscurelProcess {
    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::chat_corpus() {
        fixtures = fixtures.page(&page_id, body);
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            extra_issuers: vec![(
                bridge_key.issuer_url().to_owned(),
                bridge_key.jwks_url().to_owned(),
            )],
            bridge_kids: vec![BRIDGE_KID.to_owned()],
            ..Default::default()
        },
        fixtures: Some(fixtures.skill("memo", MEMO_SKILL).done()),
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
    _state: tempfile::TempDir,
}

impl Bridge {
    fn spawn(gw: &EscurelProcess, bridge_key: &ExtraIssuer, bf: &BotFramework) -> Self {
        let state = tempfile::tempdir().unwrap();
        let (key, kid) = bridge_key.signing_material();
        let env = [
            ("ESCUREL_BRIDGE_GATEWAY_URL", gw.base_url().to_owned()),
            ("ESCUREL_BRIDGE_TENANT", TENANT.to_owned()),
            (
                "ESCUREL_BRIDGE_AUTH_ISSUER",
                bridge_key.issuer_url().to_owned(),
            ),
            ("ESCUREL_BRIDGE_AUTH_KID", kid.to_owned()),
            ("ESCUREL_BRIDGE_AUTH_SIGNING_KEY", key),
            (
                "ESCUREL_BRIDGE_STATE_PATH",
                state.path().join("bridge.duckdb").display().to_string(),
            ),
            (
                "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE",
                "user:{aadObjectId}".to_owned(),
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
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let mut bridge = Self {
            child: None,
            listen: String::new(),
            env,
            _state: state,
        };
        bridge.start();
        bridge
    }

    fn start(&mut self) {
        self.listen = format!("127.0.0.1:{}", free_port());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-chat-bridge"));
        cmd.env("ESCUREL_BRIDGE_LISTEN", &self.listen);
        for (k, v) in &self.env {
            cmd.env(k, v);
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
    _runner: RunningRunner,
    bridge: Bridge,
    _bridge_key: ExtraIssuer,
}

async fn world(run_ms: u64) -> World {
    let bridge_key = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let gw = gateway(&bridge_key).await;
    let bf = BotFramework::start().await;
    let runner = runner(&gw, run_ms);
    let mut bridge = Bridge::spawn(&gw, &bridge_key, &bf);
    bridge.wait_healthy().await;
    World {
        gw,
        bf,
        _runner: runner,
        bridge,
        _bridge_key: bridge_key,
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
