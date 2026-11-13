//! `escurel skill export` / `skill import` against a real gateway (#825).
//!
//! A skill leaves as an Anthropic-style SKILL.md directory and comes back as
//! a DRAFT — never a direct write — that a person promotes. The round trip
//! must give back the same page: same frontmatter (escurel-only keys ride in
//! `metadata.escurel`), same body (the appended `resolve` hints are stripped).

use std::path::Path;

use assert_cmd::Command;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
kind: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
---\n\
# customer\n";

const ACME: &str = "---\n\
kind: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
---\n\
# Acme Corp\n";

/// Wikilinks in the body plus escurel-only keys a SKILL.md has no words for.
const TRIAGE_SKILL: &str = "---\n\
kind: skill\n\
id: triage\n\
description: Triage an inbound support request.\n\
summary: Sort a request by urgency and owner.\n\
autonomy: review\n\
schedule: \"0 7 * * 1-5\"\n\
tags: [support, inbox]\n\
acl:\n  read: [public]\n  create: [editors]\n\
---\n\
# triage\n\n\
Look the requester up first: [[customer::acme|Acme]] is the reference account.\n\
Compare with the [[customer]] skill's required fields before you answer.\n";

struct Harness {
    process: EscurelProcess,
    server: String,
    bearer: String,
}

async fn start() -> Harness {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .skill("triage", TRIAGE_SKILL)
                .instance("customer", "acme", ACME)
                .done(),
        ),
        config_overrides: Default::default(),
    })
    .await;
    let server = process.base_url().to_owned();
    let bearer = process.mint_token(TENANT, Role::Agent);
    Harness {
        process,
        server,
        bearer,
    }
}

/// Run the CLI; `Ok(stdout JSON)` on exit 0, `Err(stderr)` otherwise.
async fn cli(h: &Harness, args: &[&str]) -> Result<Value, String> {
    let (server, bearer) = (h.server.clone(), h.bearer.clone());
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let out = Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", server)
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .output()
            .unwrap();
        if out.status.success() {
            Ok(serde_json::from_slice(&out.stdout).expect("stdout is JSON"))
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    })
    .await
    .unwrap()
}

async fn ok(h: &Harness, args: &[&str]) -> Value {
    cli(h, args)
        .await
        .unwrap_or_else(|e| panic!("`escurel {}` failed: {e}", args.join(" ")))
}

/// A page's frontmatter as JSON (order-insensitive equality) and its body.
fn split(markdown: &str) -> (Value, String) {
    let (fields, body) = escurel_md::split_frontmatter(markdown).expect("frontmatter");
    (serde_json::to_value(fields).unwrap(), body.to_owned())
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skill_exported_as_skill_md_imports_back_as_an_equal_draft_and_promotes_unchanged() {
    let h = start().await;
    let page_id = "markdown/skills/triage.md";
    let before = ok(&h, &["page", "expand", page_id]).await;

    let out = tempfile::tempdir().unwrap();
    let exported = ok(&h, &["skill", "export", "triage", "--out", p(out.path())]).await;
    let file = out.path().join("triage").join("SKILL.md");
    assert_eq!(exported["written"][0].as_str(), Some(p(&file)));

    // What an external agent sees: SKILL.md vocabulary, escurel keys namespaced,
    // the links still readable and each one listed as a `resolve` hint.
    let skill_md = std::fs::read_to_string(&file).unwrap();
    let (fm, body) = split(&skill_md);
    assert_eq!(fm["name"], "triage");
    assert_eq!(fm["description"], "Triage an inbound support request.");
    assert!(
        fm.get("kind").is_none(),
        "no escurel page kind in a SKILL.md"
    );
    let escurel = &fm["metadata"]["escurel"];
    assert_eq!(escurel["autonomy"], "review");
    assert_eq!(escurel["schedule"], "0 7 * * 1-5");
    assert_eq!(escurel["acl"]["create"][0], "editors");
    assert!(body.contains("[[customer::acme|Acme]] is the reference account."));
    assert!(body.contains("`resolve`"), "{body}");
    assert!(body.contains("- `[[customer::acme]]`"), "{body}");
    assert!(body.contains("- `[[customer]]`"), "{body}");

    // Import: held as a draft against the stored page, not written.
    let imported = ok(&h, &["skill", "import", p(&out.path().join("triage"))]).await;
    assert_eq!(imported["ok"], true, "{imported}");
    assert_eq!(imported["target_page_id"], page_id);
    assert!(imported.get("extra_files").is_none());
    let draft = &imported["draft"];
    let (draft_fm, draft_body) = split(draft["content"].as_str().unwrap());
    let (orig_fm, orig_body) = split(TRIAGE_SKILL);
    assert_eq!(draft_fm, orig_fm, "frontmatter survives the round trip");
    assert_eq!(draft_body, orig_body, "the body survives, hints stripped");
    let listed = ok(&h, &["draft", "list"]).await;
    assert!(
        listed["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["draft_id"] == draft["draft_id"]),
        "{listed}"
    );

    // A person promotes it; the page reads exactly as before.
    let draft_id = draft["draft_id"].as_str().unwrap();
    let promoted = ok(&h, &["draft", "promote", "--draft", draft_id]).await;
    assert_eq!(promoted["ok"], true, "{promoted}");
    let after = ok(&h, &["page", "expand", page_id]).await;
    assert_eq!(after["frontmatter"], before["frontmatter"]);
    assert_eq!(after["body"], before["body"]);
    assert_eq!(after["wikilinks_out"], before["wikilinks_out"]);

    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_anthropic_skill_md_imports_as_a_minimal_skill_and_its_files_are_listed() {
    let h = start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "---\nname: pdf-tools\ndescription: Fill and merge PDF forms.\nlicense: MIT\n---\n\
         # PDF tools\n\nRun `scripts/fill.py` with the form and the values.\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("scripts")).unwrap();
    std::fs::write(dir.path().join("scripts").join("fill.py"), "print('x')\n").unwrap();

    let imported = ok(&h, &["skill", "import", p(dir.path())]).await;
    assert_eq!(imported["ok"], true, "{imported}");
    assert_eq!(imported["target_page_id"], "markdown/skills/pdf-tools.md");
    // A new skill: drafted as "expect no page".
    assert_eq!(imported["draft"]["base_sha256"], "");
    assert_eq!(imported["extra_files"][0], "scripts/fill.py");
    assert!(
        imported["extra_files_note"]
            .as_str()
            .unwrap()
            .contains("not uploaded")
    );

    // Nothing landed until it is promoted.
    let skills = ok(&h, &["skill", "list"]).await;
    let has = |v: &Value| {
        v["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "pdf-tools")
    };
    assert!(!has(&skills), "an import is a draft, not a write");
    let draft_id = imported["draft"]["draft_id"].as_str().unwrap();
    assert_eq!(
        ok(&h, &["draft", "promote", "--draft", draft_id]).await["ok"],
        true
    );
    assert!(has(&ok(&h, &["skill", "list"]).await));
    let page = ok(&h, &["page", "expand", "markdown/skills/pdf-tools.md"]).await;
    assert_eq!(page["frontmatter"]["kind"], "skill");
    assert_eq!(page["frontmatter"]["id"], "pdf-tools");
    assert_eq!(page["frontmatter"]["skill_md"]["license"], "MIT");

    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_refuses_a_name_that_is_not_a_skill_id_and_drafts_nothing() {
    let h = start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "---\nname: ../../escape\ndescription: nope\n---\nx\n",
    )
    .unwrap();
    let err = cli(&h, &["skill", "import", p(dir.path())])
        .await
        .expect_err("an unsafe name is refused");
    assert!(err.contains("is not a valid escurel skill id"), "{err}");
    assert!(
        err.contains("../../escape"),
        "the message names the value: {err}"
    );
    let drafts = ok(&h, &["draft", "list"]).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 0, "{drafts}");

    // A directory with no SKILL.md says what it was looking for.
    let empty = tempfile::tempdir().unwrap();
    let err = cli(&h, &["skill", "import", p(empty.path())])
        .await
        .unwrap_err();
    assert!(err.contains("SKILL.md"), "{err}");

    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_export_writes_one_deterministic_bundle() {
    let h = start().await;
    let out = tempfile::tempdir().unwrap();
    let args = [
        "skill",
        "export",
        "triage",
        "customer",
        "--pack",
        "--out",
        p(out.path()),
    ];
    let first = ok(&h, &args).await;
    let bundle = out.path().join("escurel-skills.tar.gz");
    assert_eq!(first["written"][0].as_str(), Some(p(&bundle)));
    let bytes = std::fs::read(&bundle).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "a gzip stream");
    ok(&h, &args).await;
    assert_eq!(
        std::fs::read(&bundle).unwrap(),
        bytes,
        "re-export is byte-identical"
    );

    let err = cli(
        &h,
        &["skill", "export", "no-such-skill", "--out", p(out.path())],
    )
    .await
    .unwrap_err();
    assert!(err.contains("no skill `no-such-skill`"), "{err}");

    h.process.shutdown().await;
}
