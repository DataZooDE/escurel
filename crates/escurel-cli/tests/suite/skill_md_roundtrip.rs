//! `escurel skill export` / `skill import` against a real gateway (#825).
//!
//! A skill leaves as an Anthropic-style SKILL.md directory and comes back as
//! a DRAFT — never a direct write — that a person promotes. The round trip
//! must give back the same page: same frontmatter (escurel-only keys ride in
//! `metadata.escurel`), same body (the appended `resolve` hints are stripped).

use std::path::Path;

use assert_cmd::Command;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

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
    start_with(Default::default()).await
}

async fn start_with(config_overrides: escurel_test_support::ConfigOverrides) -> Harness {
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
        config_overrides,
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
    cli_as(h, &h.bearer, args).await.map(|(json, _)| json)
}

/// Run the CLI with `bearer`; `Ok((stdout JSON, stderr))` on exit 0,
/// `Err(stderr)` otherwise.
async fn cli_as(h: &Harness, bearer: &str, args: &[&str]) -> Result<(Value, String), String> {
    let (server, bearer) = (h.server.clone(), bearer.to_owned());
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let out = Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", server)
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            Ok((
                serde_json::from_slice(&out.stdout).expect("stdout is JSON"),
                stderr,
            ))
        } else {
            Err(stderr)
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

/// A third-party bundle can carry authority in `metadata.escurel` (crew F13):
/// the import names those keys so the curator who promotes the draft sees
/// that it would set them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_names_the_authority_keys_its_bundle_would_set() {
    let h = start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "---\nname: playbook\ndescription: A third-party playbook.\nmetadata:\n  escurel:\n    \
         summary: s\n    acl:\n      read: [public]\n    agent: deploy-bot\n    \
         schedule: \"0 7 * * 1-5\"\n---\n# playbook\n",
    )
    .unwrap();
    let imported = ok(&h, &["skill", "import", p(dir.path())]).await;
    assert_eq!(
        imported["authority_keys"],
        json!(["acl", "agent", "schedule"]),
        "{imported}"
    );
    assert!(
        imported["authority_note"]
            .as_str()
            .is_some_and(|n| n.contains("acl, agent, schedule")),
        "{imported}"
    );
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

/// Exporting into a directory that already holds a skill never clobbers a
/// local edit (crew U-F8): identical content is a no-op, different content is
/// refused (nothing written) unless `--force`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_refuses_to_overwrite_a_local_edit_without_force() {
    let h = start().await;
    let out = tempfile::tempdir().unwrap();
    let args = [
        "skill",
        "export",
        "triage",
        "customer",
        "--out",
        p(out.path()),
    ];
    ok(&h, &args).await;
    let file = out.path().join("triage").join("SKILL.md");
    let exported = std::fs::read_to_string(&file).unwrap();

    let again = ok(&h, &args).await;
    assert_eq!(again["written"], json!([]), "{again}");
    assert_eq!(again["unchanged"].as_array().unwrap().len(), 2, "{again}");

    let edited = format!("{exported}\nA local note.\n");
    std::fs::write(&file, &edited).unwrap();
    std::fs::remove_file(out.path().join("customer").join("SKILL.md")).unwrap();
    let err = cli(&h, &args)
        .await
        .expect_err("a local edit is not clobbered");
    assert!(err.contains("--force") && err.contains(p(&file)), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);
    assert!(
        !out.path().join("customer").join("SKILL.md").exists(),
        "a refusal writes nothing at all"
    );

    let forced = ok(&h, &[&args[..], &["--force"]].concat()).await;
    assert_eq!(forced["written"].as_array().unwrap().len(), 2, "{forced}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), exported);
    h.process.shutdown().await;
}

/// A plain third-party SKILL.md imported over a governed skill would drop its
/// authority keys once promoted (crew C-F4): the import names them, on stdout
/// and as a warning on stderr.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_import_over_a_governed_skill_names_the_authority_keys_it_would_remove() {
    let h = start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "---\nname: triage\ndescription: Triage, the community version.\n---\n# triage\n",
    )
    .unwrap();
    let (imported, stderr) = cli_as(&h, &h.bearer, &["skill", "import", p(dir.path())])
        .await
        .unwrap();
    assert_eq!(
        imported["removed_authority_keys"],
        json!(["acl", "autonomy", "schedule"]),
        "{imported}"
    );
    assert!(
        stderr.contains("acl, autonomy, schedule") && stderr.contains("removes them"),
        "{stderr}"
    );
    // A new skill has nothing to lose.
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(
        fresh.path().join("SKILL.md"),
        "---\nname: fresh\ndescription: New.\n---\n# fresh\n",
    )
    .unwrap();
    let imported = ok(&h, &["skill", "import", p(fresh.path())]).await;
    assert!(
        imported.get("removed_authority_keys").is_none(),
        "{imported}"
    );
    h.process.shutdown().await;
}

/// Under an enforced write ACL (crew U-F5) a non-curator's import is a draft
/// and nothing more: the skill does not exist until someone promotes it, and
/// a curator (admin) can. The draft names the authority it would grant
/// (`agent:`). NOT pinned here: whether a non-curator may promote their own
/// import — today the gateway lets any writer promote a skill-page draft,
/// `agent:` included (reported as a finding, not changed in this PR).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_an_enforced_acl_an_import_is_a_draft_a_curator_promotes() {
    let h = start_with(escurel_test_support::ConfigOverrides {
        write_acl: Some(escurel_test_support::WriteAclMode::Enforce),
        ..Default::default()
    })
    .await;
    let bob = h
        .process
        .mint_token_with_sub(TENANT, Role::Agent, "member:bob");
    let curator = h.process.mint_token(TENANT, Role::Admin);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "---\nname: deployer\ndescription: A playbook.\nmetadata:\n  escurel:\n    \
         agent: \"[[agent::deploy-bot]]\"\n---\n# deployer\n",
    )
    .unwrap();
    let (drafted, stderr) = cli_as(&h, &bob, &["skill", "import", p(dir.path())])
        .await
        .expect("anyone may propose");
    assert_eq!(drafted["ok"], true, "{drafted}");
    assert_eq!(drafted["draft"]["author"], "member:bob", "{drafted}");
    assert_eq!(drafted["authority_keys"], json!(["agent"]), "{drafted}");
    assert!(stderr.contains("sets agent"), "{stderr}");

    let hr = &h;
    let listed = |token: String| async move {
        cli_as(hr, &token, &["skill", "list"]).await.unwrap().0["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "deployer")
    };
    assert!(!listed(curator.clone()).await, "a draft is not a write");
    let draft_id = drafted["draft"]["draft_id"].as_str().unwrap();
    let (promoted, _) = cli_as(&h, &curator, &["draft", "promote", "--draft", draft_id])
        .await
        .expect("the curator promotes");
    assert_eq!(promoted["ok"], true, "{promoted}");
    assert!(listed(curator).await);
    h.process.shutdown().await;
}
