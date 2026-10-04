//! `escurel admin migrate-kind-files`: the offline `type:` -> `kind:` migrator for repositories that
//! hold escurel pages as plain files (skills and instances in git).
//!
//! Real `escurel` binary, real temp trees, a real `git` for the dirty-tree guard: no gateway needed
//! (the command is purely local, like `admin pack verify`) and no mocks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, content: impl AsRef<[u8]>) -> PathBuf {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    p
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

/// Run the command; returns (exit code, stdout JSON or Null, stderr).
fn run(args: &[&str]) -> (i32, Value, String) {
    let out = Command::cargo_bin("escurel")
        .unwrap()
        .args(["admin", "migrate-kind-files"])
        .args(args)
        .output()
        .unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (
        out.status.code().unwrap_or(-1),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn paths(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}

const SKILL: &str = "---\ntype: skill\nid: customer\ndescription: A buyer.\n---\n\n# Customer\n";
const INSTANCE: &str = "---\ntype: instance\nskill: customer\nid: acme\n---\n\n# Acme\n";

#[test]
fn dry_run_is_the_default_and_writes_nothing() {
    let d = TempDir::new().unwrap();
    let a = write(d.path(), "skills/customer.md", SKILL);
    let b = write(d.path(), "instances/customer/acme.md", INSTANCE);

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["summary"]["to_migrate"], 2);
    assert_eq!(read(&a), SKILL, "a dry run must not touch a file");
    assert_eq!(read(&b), INSTANCE);

    // A unified-diff style hunk per page: what would change, and where.
    let first = &v["migrate"][0];
    let diff = first["diff"].as_str().unwrap();
    assert!(
        diff.contains("-type: skill") || diff.contains("-type: instance"),
        "{diff}"
    );
    assert!(
        diff.contains("+kind: skill") || diff.contains("+kind: instance"),
        "{diff}"
    );
    assert!(diff.contains("@@ line 2 @@"), "{diff}");
}

#[test]
fn apply_rewrites_nested_trees_and_a_second_run_changes_nothing() {
    let d = TempDir::new().unwrap();
    let a = write(d.path(), "skills/customer.md", SKILL);
    let deep = write(d.path(), "a/b/c/d/instances/customer/acme.md", INSTANCE);

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["summary"]["to_migrate"], 2);
    assert!(read(&a).starts_with("---\nkind: skill\n"));
    assert!(read(&deep).starts_with("---\nkind: instance\n"));

    let before = (read(&a), read(&deep));
    let (code, v, _) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0);
    assert_eq!(
        v["summary"]["to_migrate"], 0,
        "idempotent: nothing left to do"
    );
    assert_eq!(v["summary"]["already_kind"], 2);
    assert_eq!((read(&a), read(&deep)), before);
}

#[test]
fn only_the_page_kind_line_changes_and_fences_and_prose_are_left_alone() {
    let d = TempDir::new().unwrap();
    let page = "---\n# a comment\ntype: skill # trailing comment\nid: x\nfields:\n  - {name: type, kind: string}\n---\n\nProse says type: skill too.\n\n```yaml\n---\ntype: skill\n---\n```\n";
    let p = write(d.path(), "skills/x.md", page);
    let (code, _, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    let expected = page.replacen(
        "type: skill # trailing comment",
        "kind: skill # trailing comment",
        1,
    );
    assert_eq!(
        read(&p),
        expected,
        "exactly one line changes, every other byte is preserved"
    );
}

#[test]
fn crlf_line_endings_on_the_edited_line_are_kept() {
    let d = TempDir::new().unwrap();
    let p = write(
        d.path(),
        "skills/x.md",
        "---\ntype: skill\r\nid: a\r\n---\r\nbody\r\n",
    );
    let (code, _, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(read(&p), "---\nkind: skill\r\nid: a\r\n---\r\nbody\r\n");
}

#[test]
fn bom_and_crlf_opening_pages_are_reported_for_manual_fixing_never_rewritten() {
    // The engine's parser requires the page to start with exactly `---\n`, so these pages are
    // unparseable TODAY, whatever their key says: rewriting the key would not make them valid.
    let d = TempDir::new().unwrap();
    let bom = write(
        d.path(),
        "skills/bom.md",
        "\u{feff}---\ntype: skill\nid: bom\n---\n",
    );
    let crlf = write(
        d.path(),
        "skills/crlf.md",
        "---\r\ntype: skill\r\nid: crlf\r\n---\r\n",
    );

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["needs_manual"], 2);
    assert_eq!(v["summary"]["to_migrate"], 0);
    let reasons: Vec<(String, String)> = v["needs_manual"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["path"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned(),
                e["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        reasons.contains(&("bom.md".into(), "bom".into())),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&("crlf.md".into(), "crlf_open".into())),
        "{reasons:?}"
    );
    assert_eq!(read(&bom), "\u{feff}---\ntype: skill\nid: bom\n---\n");
    assert_eq!(read(&crlf), "---\r\ntype: skill\r\nid: crlf\r\n---\r\n");
}

#[test]
fn a_page_with_both_keys_is_a_conflict_and_is_never_touched() {
    let d = TempDir::new().unwrap();
    let both = "---\ntype: skill\nkind: skill\nid: x\n---\n";
    let p = write(d.path(), "skills/x.md", both);
    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["conflicts"], 1);
    assert_eq!(paths(&v, "conflicts").len(), 1);
    assert_eq!(read(&p), both);
}

#[test]
fn a_data_field_named_kind_is_explained_so_the_owner_knows_what_to_rename() {
    // Real finding from the consumer repos: herkules code skills carry `kind: code`, datazoo-loops
    // `system` instances carry `kind: saas-api`. After the cut `kind:` IS the page kind, so the data
    // field must be renamed first (the engine's own `issue` skill became `issue_kind`).
    let d = TempDir::new().unwrap();
    let p = write(
        d.path(),
        "skills/tabelle.md",
        "---\ntype: skill\nid: tabelle\nkind: code\n---\n",
    );
    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    let reason = v["conflicts"][0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("kind: code"),
        "names the colliding value: {reason}"
    );
    assert!(
        reason.contains("rename") && reason.contains("_kind"),
        "says what to do: {reason}"
    );
    assert_eq!(read(&p), "---\ntype: skill\nid: tabelle\nkind: code\n---\n");
}

#[test]
fn a_user_data_field_named_type_is_never_rewritten_and_is_listed() {
    let d = TempDir::new().unwrap();
    let inst = "---\nkind: instance\nskill: invoice\nid: i1\ntype: credit-note\n---\n";
    let legacy_other = "---\ntype: invoice\nid: i2\n---\n"; // no page kind at all, a data field
    let a = write(d.path(), "instances/invoice/i1.md", inst);
    let b = write(d.path(), "instances/invoice/i2.md", legacy_other);
    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["to_migrate"], 0);
    assert_eq!(read(&a), inst);
    assert_eq!(read(&b), legacy_other);
    let listed: Vec<String> = v["untouched_type_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{}={}",
                e["path"].as_str().unwrap().rsplit('/').next().unwrap(),
                e["value"].as_str().unwrap()
            )
        })
        .collect();
    assert!(
        listed.contains(&"i1.md=credit-note".to_owned()),
        "{listed:?}"
    );
    assert!(listed.contains(&"i2.md=invoice".to_owned()), "{listed:?}");
}

#[test]
fn only_markdown_files_are_touched_and_vendor_dirs_are_skipped() {
    let d = TempDir::new().unwrap();
    let txt = write(d.path(), "notes.txt", SKILL);
    let json = write(d.path(), "data.json", "{\"x\": \"type: skill\"}");
    let alt = write(d.path(), "skills/y.markdown", SKILL);
    let git = write(d.path(), ".git/hooks/x.md", SKILL);
    let node = write(d.path(), "node_modules/pkg/skill.md", SKILL);
    let readme = write(d.path(), "README.md", "# No frontmatter\n\ntype: skill\n");
    let ok = write(d.path(), "skills/ok.md", SKILL);

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["to_migrate"], 1);
    for p in [&txt, &alt, &git, &node] {
        assert_eq!(read(p), SKILL, "{} must not be touched", p.display());
    }
    assert_eq!(read(&json), "{\"x\": \"type: skill\"}");
    assert_eq!(read(&readme), "# No frontmatter\n\ntype: skill\n");
    assert!(read(&ok).starts_with("---\nkind: skill\n"));
    assert_eq!(v["summary"]["no_frontmatter"], 1);
}

#[test]
fn signed_pack_base_pages_are_skipped_and_reported() {
    let d = TempDir::new().unwrap();
    let base = write(d.path(), "markdown/base/crm-pack/skills/lead.md", SKILL);
    let overlay = write(d.path(), "markdown/skills/own.md", SKILL);
    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["skipped_signed_pack"], 1);
    assert_eq!(
        read(&base),
        SKILL,
        "a signed pack page is the publisher's to re-export"
    );
    assert!(read(&overlay).starts_with("---\nkind: skill\n"));
}

#[test]
fn huge_pages_are_handled_and_an_over_limit_file_is_skipped_not_read() {
    let d = TempDir::new().unwrap();
    let body = "lorem ipsum dolor sit amet\n".repeat(800_000); // ~21 MB
    let big = format!("---\ntype: instance\nskill: c\nid: big\n---\n{body}");
    let p = write(d.path(), "instances/c/big.md", &big);
    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["to_migrate"], 1);
    assert_eq!(
        read(&p),
        big.replacen("type: instance", "kind: instance", 1)
    );

    let d2 = TempDir::new().unwrap();
    let huge = format!(
        "---\ntype: instance\nskill: c\nid: h\n---\n{}",
        "x".repeat(3 * 1024 * 1024)
    );
    let q = write(d2.path(), "instances/c/h.md", &huge);
    let (code, v, err) = run(&[
        "--path",
        d2.path().to_str().unwrap(),
        "--apply",
        "--max-bytes",
        "1048576",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["skipped_too_large"], 1);
    assert_eq!(read(&q), huge);
}

fn git(dir: &Path, args: &[&str]) {
    let out = Proc::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_dirty_git_tree_refuses_apply_unless_allowed_but_a_dry_run_is_fine() {
    let d = TempDir::new().unwrap();
    git(d.path(), &["init", "-q"]);
    let page = write(d.path(), "skills/x.md", SKILL);
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);

    // Clean tree: apply works.
    let (code, _, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert!(read(&page).starts_with("---\nkind: skill\n"));

    // Dirty tree (an unrelated uncommitted edit): apply refuses and names the flag.
    git(d.path(), &["checkout", "-q", "--", "."]);
    write(d.path(), "wip.md", "work in progress\n");
    let (code, _, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_ne!(code, 0);
    assert!(err.contains("--allow-dirty"), "{err}");
    assert_eq!(read(&page), SKILL, "a refused apply writes nothing");

    let (code, v, _) = run(&["--path", d.path().to_str().unwrap()]);
    assert_eq!(code, 0, "a dry run never needs a clean tree");
    assert_eq!(v["summary"]["to_migrate"], 1);

    let (code, _, err) = run(&[
        "--path",
        d.path().to_str().unwrap(),
        "--apply",
        "--allow-dirty",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(read(&page).starts_with("---\nkind: skill\n"));
}

#[test]
fn run_board_status_is_renamed_and_string_actions_are_reported_not_converted() {
    let d = TempDir::new().unwrap();
    let board = "---\nkind: instance\nskill: workflow-run\nid: r1\nstatus: running\n---\n";
    let other = "---\nkind: instance\nskill: ticket\nid: t1\nstatus: open\n---\n";
    let acts = "---\nkind: skill\nid: supplier-risk\nactions: [customer-notice, confirmation-request]\n---\n";
    let ok_acts =
        "---\nkind: skill\nid: ok\nactions:\n  - {name: a, kind: event, label: A, event: x}\n---\n";
    let b = write(d.path(), "instances/workflow-run/r1.md", board);
    let o = write(d.path(), "instances/ticket/t1.md", other);
    let a = write(d.path(), "skills/supplier-risk.md", acts);
    write(d.path(), "skills/ok.md", ok_acts);

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        read(&b),
        board.replacen("status: running", "run_status: running", 1)
    );
    assert_eq!(read(&o), other, "a tenant's own status field is data");
    assert_eq!(
        read(&a),
        acts,
        "string actions need a human to write labels: reported only"
    );
    assert_eq!(v["summary"]["run_status"], 1);
    let flagged = paths(&v, "legacy_string_actions");
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    assert!(flagged[0].ends_with("skills/supplier-risk.md"));
}

#[test]
fn unreadable_files_are_reported_and_symlinks_are_not_followed() {
    let d = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let out_page = write(outside.path(), "skills/out.md", SKILL);
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
    write(d.path(), "skills/bad.md", [0xff, 0xfe, b'-', b'-', 0xff]);
    let ok = write(d.path(), "skills/ok.md", SKILL);

    let (code, v, err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["unreadable"], 1);
    assert_eq!(v["summary"]["to_migrate"], 1);
    assert!(read(&ok).starts_with("---\nkind: skill\n"));
    assert_eq!(
        read(&out_page),
        SKILL,
        "a symlinked directory is never followed"
    );
}

#[test]
fn several_roots_and_a_missing_root() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    write(a.path(), "skills/x.md", SKILL);
    write(b.path(), "skills/y.md", SKILL);
    let (code, v, err) = run(&[
        "--path",
        a.path().to_str().unwrap(),
        "--path",
        b.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["to_migrate"], 2);

    let (code, _, err) = run(&["--path", "/definitely/not/here"]);
    assert_ne!(code, 0);
    assert!(err.contains("/definitely/not/here"), "{err}");
}

#[test]
fn a_nested_repository_or_submodule_is_skipped_and_reported_unless_asked() {
    // `vendor/lib` is a git submodule (its `.git` is a FILE pointing at the superproject's module
    // dir); `third_party/x` is a nested clone (a `.git` DIRECTORY). Their pages belong to ANOTHER
    // repository: migrating them here would edit a different history.
    let d = TempDir::new().unwrap();
    let own = write(d.path(), "skills/own.md", SKILL);
    write(
        d.path(),
        "vendor/lib/.git",
        "gitdir: ../../.git/modules/lib\n",
    );
    let sub = write(d.path(), "vendor/lib/skills/s.md", SKILL);
    fs::create_dir_all(d.path().join("third_party/x/.git")).unwrap();
    let nested = write(d.path(), "third_party/x/skills/n.md", SKILL);

    let (code, v, err) = run(&[
        "--path",
        d.path().to_str().unwrap(),
        "--apply",
        "--allow-dirty",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        v["summary"]["to_migrate"], 1,
        "only this repository's own pages"
    );
    assert_eq!(v["summary"]["skipped_nested_repo"], 2);
    assert!(read(&own).starts_with("---\nkind: skill\n"));
    assert_eq!(read(&sub), SKILL);
    assert_eq!(read(&nested), SKILL);
    let nested_paths: Vec<String> = v["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["reason"] == "nested_repo")
        .map(|s| s["path"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        nested_paths.iter().any(|p| p.ends_with("vendor/lib")),
        "{nested_paths:?}"
    );
    assert!(
        nested_paths.iter().any(|p| p.ends_with("third_party/x")),
        "{nested_paths:?}"
    );

    // Explicitly opting in migrates them too (a repo that vendors pages it owns).
    let (code, v, err) = run(&[
        "--path",
        d.path().to_str().unwrap(),
        "--apply",
        "--allow-dirty",
        "--include-nested-repos",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["summary"]["to_migrate"], 2);
    assert!(read(&sub).starts_with("---\nkind: skill\n"));
}

/// A planted symlink at the temp file's name must not turn the migrator into an arbitrary-file
/// overwrite: the temp used to be created with `fs::write`, which follows a link.
#[test]
fn apply_does_not_write_through_a_planted_symlink() {
    let d = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let victim = write(outside.path(), "victim.txt", "DO NOT TOUCH");
    let page = write(d.path(), "skills/customer.md", SKILL);
    std::os::unix::fs::symlink(&victim, d.path().join("skills/customer.md.kindfix.tmp")).unwrap();

    let (_code, _v, _err) = run(&["--path", d.path().to_str().unwrap(), "--apply"]);

    assert_eq!(
        read(&victim),
        "DO NOT TOUCH",
        "the link target was written through"
    );
    // The migration itself still happened (or was cleanly refused): never half-applied.
    let migrated = read(&page);
    assert!(
        migrated == SKILL || migrated.contains("kind: skill"),
        "{migrated}"
    );
}
