//! The egress policy over a SQL source's connection string, probed through a real gateway (default,
//! strict policy) the way a tenant admin would: register a credential, point a skill at it, read.
//!
//! The first version of the check split the DSN on whitespace and `=` and judged only the first host,
//! so libpq spellings the driver honours — `host = x` with spaces, `hostaddr=` in a URI's query,
//! host lists, `service=` / `passfile=` — walked a private address (or a local file) past it. Every
//! probe here must be refused BEFORE a connection is attempted, with the policy's own words, and the
//! answer must never echo the connection string.

use escurel_test_support::{AuthMode, EscurelProcess, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

fn skill(attach: &str) -> String {
    format!(
        "---\nkind: skill\nid: probe\ndescription: probe.\nfields:\n  - {{name: id, kind: string, required: true}}\n\
         backend:\n  kind: sql_view\n  instances: rows\n  key: id\n  linked: markdown\n  \
         source: {{connector: postgres, attach: {attach}, relation: \"public.t\"}}\n  project: {{id: id}}\n---\n# probe\n"
    )
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", p.mint_token(TENANT, Role::Admin)),
        )
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn libpq_spellings_that_hide_a_private_host_are_refused_before_any_connection() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        ..Default::default()
    })
    .await;
    // A public address as the decoy; the private one is where the driver would actually go.
    let probes = [
        (
            "spaces",
            "host=93.184.216.34 host = 127.0.0.1 dbname=x connect_timeout=2 password=hunter2",
        ),
        (
            "uri_hostaddr",
            "postgresql://u:hunter2@93.184.216.34/db?hostaddr=127.0.0.1&options=-cdatestyle%3Diso&connect_timeout=2",
        ),
        (
            "list",
            "host=93.184.216.34,10.0.0.5 dbname=x connect_timeout=2 password=hunter2",
        ),
        ("service", "service=prod dbname=x password=hunter2"),
        (
            "passfile",
            "host=93.184.216.34 passfile=/etc/passwd password=hunter2",
        ),
    ];
    for (name, dsn) in probes {
        let cred = format!("c_{name}");
        let reg = call(
            &p,
            "register_credential",
            json!({ "name": cred, "connector": "postgres", "secret": dsn }),
        )
        .await;
        let _ = reg;
        let w = call(
            &p,
            "update_page",
            json!({ "page_id": "markdown/skills/probe.md", "content": skill(&cred) }),
        )
        .await;
        let _ = w;
        let started = std::time::Instant::now();
        let out = call(
            &p,
            "list_instances",
            json!({ "skill": "probe", "limit": 3 }),
        )
        .await;
        let text = out.to_string();
        assert!(
            text.contains("egress policy"),
            "{name}: the policy refuses it, in its own words: {text}"
        );
        assert!(
            !text.contains("hunter2"),
            "{name}: the answer never echoes the connection string: {text}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{name}: refused before a connection attempt ({:?})",
            started.elapsed()
        );
    }
}
