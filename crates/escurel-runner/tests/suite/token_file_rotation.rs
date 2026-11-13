//! `ESCUREL_RUNNER_TOKEN_FILE` (crew U-F6): a laptop or on-prem runner on a
//! short-lived service credential, kept fresh in a file by a sidecar.
//!
//! Real gateway, real runner. The runner starts on a credential the gateway
//! refuses (`401` on every call: it files nothing), then the file is
//! rewritten with a good one — and the same process, never restarted, runs
//! the next event.

use std::time::Duration;

use base64::Engine as _;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, RunnerProcess};
use serde_json::json;

use crate::support::call;

const TENANT: &str = "acme";
const NOTE: &str =
    "---\nkind: skill\nid: note\nautonomy: auto\n---\n# note\n\nFold the event in.\n";

/// A JWT-shaped bearer for `sub` the gateway cannot verify (no valid
/// signature): what an expired or revoked credential looks like to it.
fn refused_bearer(sub: &str) -> String {
    let b64 = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    };
    format!(
        "{}.{}.c2lnbmF0dXJl",
        b64(json!({ "alg": "RS256", "typ": "JWT" })),
        b64(json!({ "sub": sub, "exp": 1 }))
    )
}

#[tokio::test]
async fn a_rotated_token_file_is_picked_up_without_a_restart() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance(
                    "note",
                    "c1",
                    "---\nkind: instance\nid: c1\nskill: note\n---\n# C1\n\nBASELINE.\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("token");
    std::fs::write(&file, format!("{}\n", refused_bearer("runner:laptop"))).unwrap();
    let mut runner = RunnerProcess::echo(&gw, TENANT).token_file(&file).spawn();
    runner.wait_healthy().await;

    let capture = |body: &'static str| {
        let (gw, admin) = (&gw, admin.clone());
        async move {
            call(
                gw,
                &admin,
                "capture_event",
                json!({ "source": "test", "mime": "text/plain", "label_skill": "note",
                        "instance_page_id": "markdown/instances/note/c1.md",
                        "title": "t", "body": body }),
            )
            .await["event_id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let first = capture("before the rotation").await;
    // The refused credential reads nothing: a few polls go by with no run.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        runner.debug_run(&first).await.is_none(),
        "it had no credential"
    );

    // The sidecar rotates the credential; the runner is not restarted.
    std::fs::write(
        &file,
        gw.mint_token_with_sub(TENANT, Role::Admin, "runner:laptop"),
    )
    .unwrap();
    for e in [first, capture("after the rotation").await] {
        runner
            .wait_for_status(&e, &["processed"], Duration::from_secs(30))
            .await;
    }
    assert!(runner.exited().is_none(), "the same process recovered");
}
