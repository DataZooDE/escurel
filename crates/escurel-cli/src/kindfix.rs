//! `escurel admin migrate-kind-files`: the OFFLINE `type:` -> `kind:` migrator for repositories that
//! hold escurel pages as plain markdown files (skills and instances in git), the file-tree twin of
//! the tenant-side `admin migrate-kind`.
//!
//! It is purely local (no gateway, like `admin pack verify`) and reuses the engine's own text
//! edits ([`escurel_md::rewrite_legacy_type_key`], [`escurel_md::rewrite_workflow_run_status`]) so a
//! repository migrates exactly the way a tenant does: one frontmatter line changes, every other
//! byte is preserved, a page with both keys is a conflict, a user's own data field named `type:`
//! is never touched.
//!
//! Safety rules, all pinned by `tests/suite/cli_kindfix_e2e.rs`:
//! - a DRY RUN unless `--apply`; the report carries a unified-diff style hunk per changed page;
//! - `--apply` refuses a dirty git working tree unless `--allow-dirty`;
//! - only `*.md` files are read; symlinks are never followed; `.git`, `node_modules`, `target`,
//!   `.dart_tool`, `.venv` are skipped; a nested git repository or submodule is not entered
//!   (`--include-nested-repos` opts in), because its pages belong to another history; signed pack pages (`markdown/base/**`) are the publisher's
//!   to re-export and are reported, never rewritten;
//! - pages the ENGINE cannot parse today (a BOM or a CRLF opening `---` line) are reported as
//!   `needs_manual` and never rewritten: changing their key would not make them valid;
//! - a write is atomic (temp file + rename) and is refused if the file changed since it was read;
//! - idempotent: a second run finds nothing to do.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;
use escurel_md::{KindRewrite, rewrite_legacy_type_key, rewrite_workflow_run_status};
use serde_json::{Value, json};

/// Directory names never entered.
const SKIP_DIRS: [&str; 5] = [".git", "node_modules", "target", ".dart_tool", ".venv"];

#[derive(Args, Debug)]
pub struct MigrateKindFilesArgs {
    /// A directory tree of escurel pages (repeatable).
    #[arg(long = "path", required = true)]
    pub paths: Vec<PathBuf>,
    /// Write the changes (default: a dry run that reports what would change).
    #[arg(long)]
    pub apply: bool,
    /// Allow `--apply` on a git working tree with uncommitted changes.
    #[arg(long)]
    pub allow_dirty: bool,
    /// Files larger than this are skipped and reported (bytes).
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    pub max_bytes: u64,
    /// Also migrate pages inside nested git repositories and submodules (skipped by default: they
    /// belong to another repository's history).
    #[arg(long)]
    pub include_nested_repos: bool,
}

/// What one file turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Rewrite it to `new`; `kind_changed` / `run_status_changed` say which edits it carries.
    Rewrite {
        new: String,
        kind_changed: bool,
        run_status_changed: bool,
    },
    AlreadyKind,
    Conflict,
    /// Cannot be parsed by the engine today (`bom` | `crlf_open`); reported, never rewritten.
    NeedsManual(&'static str),
    /// Has frontmatter but no page-kind key at all.
    NoPageKind,
    NoFrontmatter,
}

/// Classify one page's text. Pure: the whole migration policy lives here.
#[must_use]
pub fn classify(text: &str) -> Verdict {
    if !text.starts_with("---\n") {
        // A page the engine's parser would refuse outright. If it is clearly a page (a frontmatter
        // block after a BOM / CRLF opening), say so instead of silently ignoring it.
        let (reason, normalised) = if let Some(rest) = text.strip_prefix('\u{feff}') {
            ("bom", rest.replacen("---\r\n", "---\n", 1))
        } else if text.starts_with("---\r\n") {
            ("crlf_open", text.replacen("---\r\n", "---\n", 1))
        } else {
            return Verdict::NoFrontmatter;
        };
        return match rewrite_legacy_type_key(&normalised) {
            KindRewrite::NotAPageKind => Verdict::NoFrontmatter,
            _ => Verdict::NeedsManual(reason),
        };
    }
    let mut new = text.to_owned();
    let mut kind_changed = false;
    match rewrite_legacy_type_key(text) {
        KindRewrite::Rewritten(s) => {
            new = s;
            kind_changed = true;
        }
        KindRewrite::Conflict => return Verdict::Conflict,
        KindRewrite::AlreadyKind => {}
        KindRewrite::NotAPageKind => {
            return if frontmatter_closed(text) {
                Verdict::NoPageKind
            } else {
                Verdict::NoFrontmatter
            };
        }
    }
    let mut run_status_changed = false;
    if let Some(s) = rewrite_workflow_run_status(&new) {
        new = s;
        run_status_changed = true;
    }
    if kind_changed || run_status_changed {
        Verdict::Rewrite {
            new,
            kind_changed,
            run_status_changed,
        }
    } else {
        Verdict::AlreadyKind
    }
}

fn frontmatter_closed(text: &str) -> bool {
    text.split_inclusive('\n')
        .skip(1)
        .any(|l| l.trim_end_matches(['\r', '\n']) == "---")
}

/// The values of top-level `type:` lines in the frontmatter that are NOT the page kind: a user's
/// own data field. Reported so an owner can see exactly what the migration deliberately left.
#[must_use]
pub fn data_type_fields(text: &str) -> Vec<String> {
    if !text.starts_with("---\n") {
        return vec![];
    }
    let mut out = vec![];
    for line in text.split_inclusive('\n').skip(1) {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            break;
        }
        if let Some(rest) = content.strip_prefix("type:") {
            let v = rest
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            if v != "skill" && v != "instance" {
                out.push(v.to_owned());
            }
        }
    }
    out
}

/// `true` when a SKILL page still lists `actions:` as plain skill ids (the removed string form).
/// They cannot be converted mechanically (an action needs a label), so they are only reported.
#[must_use]
pub fn has_string_actions(text: &str) -> bool {
    let Ok(page) = escurel_md::parse(text) else {
        return false;
    };
    page.frontmatter.page_kind == escurel_md::PageKind::Skill
        && page
            .frontmatter
            .fields
            .get("actions")
            .and_then(|v| v.as_sequence())
            .is_some_and(|items| items.iter().any(escurel_md::YamlValue::is_string))
}

/// A unified-diff style description of the lines that differ (the edits never change the line
/// count, so a positional comparison is exact).
fn diff(old: &str, new: &str) -> String {
    let mut out = String::new();
    for (i, (a, b)) in old.split('\n').zip(new.split('\n')).enumerate() {
        if a != b {
            out.push_str(&format!(
                "@@ line {} @@\n-{}\n+{}\n",
                i + 1,
                a.trim_end_matches('\r'),
                b.trim_end_matches('\r')
            ));
        }
    }
    out
}

/// What a directory walk found: the markdown files, and the nested repositories it did not enter.
struct Walk {
    files: Vec<PathBuf>,
    nested_repos: Vec<PathBuf>,
}

/// Every `*.md` file under `root` (sorted, symlinks never followed, vendor dirs skipped). A nested
/// git repository (a directory holding its own `.git` file or directory, e.g. a submodule) is not
/// entered unless `include_nested`: its pages belong to another history.
fn markdown_files(root: &Path, include_nested: bool) -> Result<Walk> {
    let mut files = vec![];
    let mut nested_repos = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir != root && !include_nested && dir.join(".git").exists() {
            nested_repos.push(dir);
            continue;
        }
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .with_context(|| format!("reading directory {}", dir.display()))?
            .collect::<std::io::Result<_>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let path = e.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let name = e.file_name();
                if !SKIP_DIRS.iter().any(|s| name == *s) {
                    stack.push(path);
                }
            } else if meta.is_file() && path.extension().is_some_and(|x| x == "md") {
                files.push(path);
            }
        }
    }
    files.sort();
    nested_repos.sort();
    Ok(Walk {
        files,
        nested_repos,
    })
}

/// `markdown/base/<pack>/...`: a signed pack's pages.
fn is_signed_pack_page(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str()).collect();
    parts
        .windows(2)
        .any(|w| w[0] == "markdown" && w[1] == "base")
}

/// `Some(count)` of uncommitted entries when `dir` is inside a git work tree.
fn git_dirty(dir: &Path) -> Option<usize> {
    let inside = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()?;
    if !inside.status.success() {
        return None;
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&status.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}

/// Atomic in-place write, refused when the file changed since it was read.
fn write_if_unchanged(path: &Path, original: &str, new: &str) -> Result<()> {
    let current = fs::read(path)?;
    if current != original.as_bytes() {
        bail!("{} changed while it was being migrated", path.display());
    }
    let tmp = path.with_extension("md.kindfix.tmp");
    fs::write(&tmp, new)?;
    fs::set_permissions(&tmp, fs::metadata(path)?.permissions())?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Run the migration over the given roots. Returns the JSON report; `Err` means nothing was
/// migrated beyond what the message says.
pub fn run(args: MigrateKindFilesArgs) -> Result<Value> {
    let mut roots = vec![];
    for p in &args.paths {
        if !p.is_dir() {
            bail!("path does not exist or is not a directory: {}", p.display());
        }
        if !roots.contains(p) {
            roots.push(p.clone());
        }
    }

    let mut git_report = vec![];
    for root in &roots {
        let dirty = git_dirty(root);
        git_report.push(json!({
            "root": root.display().to_string(),
            "git_repo": dirty.is_some(),
            "uncommitted_entries": dirty.unwrap_or(0),
        }));
        if args.apply && !args.allow_dirty && dirty.is_some_and(|n| n > 0) {
            bail!(
                "{} has {} uncommitted change(s): commit or stash them so the migration is one \
                 reviewable diff, or pass --allow-dirty",
                root.display(),
                dirty.unwrap_or(0)
            );
        }
    }

    let mut migrate = vec![];
    let mut run_status = vec![];
    let mut conflicts = vec![];
    let mut needs_manual = vec![];
    let mut skipped = vec![];
    let mut untouched_type_fields = vec![];
    let mut legacy_string_actions = vec![];
    let mut errors = vec![];
    let (mut scanned, mut already_kind, mut no_frontmatter, mut no_page_kind) = (0, 0, 0, 0);
    let mut unreadable = 0;

    for root in &roots {
        let walk = markdown_files(root, args.include_nested_repos)?;
        for repo in &walk.nested_repos {
            skipped.push(json!({"path": repo.display().to_string(), "reason": "nested_repo"}));
        }
        for path in walk.files {
            scanned += 1;
            let shown = path.display().to_string();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if is_signed_pack_page(rel) {
                skipped.push(json!({"path": shown, "reason": "signed_pack"}));
                continue;
            }
            if fs::metadata(&path)?.len() > args.max_bytes {
                skipped.push(json!({"path": shown, "reason": "too_large"}));
                continue;
            }
            let Ok(text) = String::from_utf8(fs::read(&path)?) else {
                unreadable += 1;
                skipped.push(json!({"path": shown, "reason": "unreadable_not_utf8"}));
                continue;
            };
            for v in data_type_fields(&text) {
                untouched_type_fields.push(json!({"path": shown, "value": v}));
            }
            match classify(&text) {
                Verdict::Rewrite {
                    new,
                    kind_changed,
                    run_status_changed,
                } => {
                    let d = diff(&text, &new);
                    if kind_changed {
                        migrate.push(json!({"path": shown, "diff": d}));
                    }
                    if run_status_changed {
                        run_status.push(json!({"path": shown, "diff": d}));
                    }
                    if has_string_actions(&new) {
                        legacy_string_actions.push(json!({"path": shown}));
                    }
                    if args.apply
                        && let Err(e) = write_if_unchanged(&path, &text, &new)
                    {
                        errors.push(format!("{shown}: {e:#}"));
                    }
                }
                Verdict::AlreadyKind => {
                    already_kind += 1;
                    if has_string_actions(&text) {
                        legacy_string_actions.push(json!({"path": shown}));
                    }
                }
                Verdict::Conflict => conflicts.push(json!({
                    "path": shown,
                    "reason": "both `type:` and `kind:` are present; resolve by hand"
                })),
                Verdict::NeedsManual(reason) => needs_manual.push(json!({
                    "path": shown,
                    "reason": reason,
                    "detail": "the engine cannot parse a page that does not start with exactly `---\\n`; fix the file's encoding/line ending first"
                })),
                Verdict::NoPageKind => no_page_kind += 1,
                Verdict::NoFrontmatter => no_frontmatter += 1,
            }
        }
    }

    let summary = json!({
        "files_scanned": scanned,
        "to_migrate": migrate.len(),
        "run_status": run_status.len(),
        "already_kind": already_kind,
        "conflicts": conflicts.len(),
        "needs_manual": needs_manual.len(),
        "skipped_signed_pack": skipped.iter().filter(|s| s["reason"] == "signed_pack").count(),
        "skipped_too_large": skipped.iter().filter(|s| s["reason"] == "too_large").count(),
        "skipped_nested_repo": skipped.iter().filter(|s| s["reason"] == "nested_repo").count(),
        "unreadable": unreadable,
        "no_frontmatter": no_frontmatter,
        "no_page_kind": no_page_kind,
        "untouched_type_fields": untouched_type_fields.len(),
        "legacy_string_actions": legacy_string_actions.len(),
    });
    let report = json!({
        "dry_run": !args.apply,
        "roots": roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>(),
        "git": git_report,
        "summary": summary,
        "migrate": migrate,
        "run_status": run_status,
        "conflicts": conflicts,
        "needs_manual": needs_manual,
        "skipped": skipped,
        "untouched_type_fields": untouched_type_fields,
        "legacy_string_actions": legacy_string_actions,
    });
    if !errors.is_empty() {
        bail!(
            "{} file(s) could not be written ({}); every other page was migrated and a re-run \
             is idempotent",
            errors.len(),
            errors.join("; ")
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rewrites_only_a_legacy_page_kind_and_reports_the_rest() {
        assert!(matches!(
            classify("---\ntype: skill\nid: a\n---\n"),
            Verdict::Rewrite {
                kind_changed: true,
                run_status_changed: false,
                ..
            }
        ));
        assert_eq!(
            classify("---\nkind: skill\nid: a\n---\n"),
            Verdict::AlreadyKind
        );
        assert_eq!(
            classify("---\ntype: skill\nkind: skill\n---\n"),
            Verdict::Conflict
        );
        assert_eq!(classify("---\nid: a\n---\n"), Verdict::NoPageKind);
        assert_eq!(classify("# just prose\n"), Verdict::NoFrontmatter);
        assert_eq!(
            classify("\u{feff}---\ntype: skill\n---\n"),
            Verdict::NeedsManual("bom")
        );
        assert_eq!(
            classify("---\r\nkind: skill\r\n---\r\n"),
            Verdict::NeedsManual("crlf_open")
        );
        // An unterminated block is not a page.
        assert_eq!(classify("---\ntype: skill\n"), Verdict::NoFrontmatter);
    }

    #[test]
    fn diff_names_the_changed_line_and_nothing_else() {
        let d = diff(
            "---\ntype: skill\nid: a\n---\n",
            "---\nkind: skill\nid: a\n---\n",
        );
        assert_eq!(d, "@@ line 2 @@\n-type: skill\n+kind: skill\n");
    }

    #[test]
    fn data_type_fields_ignores_the_page_kind_and_nested_keys() {
        let t = "---\ntype: skill\nfields:\n  - {name: x}\n  type: nested\n---\n";
        assert!(data_type_fields(t).is_empty());
        assert_eq!(
            data_type_fields("---\nkind: instance\ntype: \"credit-note\" # why\n---\n"),
            vec!["credit-note".to_owned()]
        );
    }

    #[test]
    fn a_write_is_refused_when_the_file_changed_since_it_was_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.md");
        fs::write(&p, "---\ntype: skill\n---\nread this\n").unwrap();
        let original = fs::read_to_string(&p).unwrap();
        // Someone edits the file after we read it.
        fs::write(&p, "---\ntype: skill\n---\nedited meanwhile\n").unwrap();
        let err =
            write_if_unchanged(&p, &original, "---\nkind: skill\n---\nread this\n").unwrap_err();
        assert!(err.to_string().contains("changed while"), "{err}");
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "---\ntype: skill\n---\nedited meanwhile\n"
        );
        assert!(
            !d.path().join("a.md.kindfix.tmp").exists(),
            "no temp file is left behind"
        );
    }

    #[test]
    fn a_signed_pack_page_is_recognised_by_its_path() {
        assert!(is_signed_pack_page(Path::new(
            "markdown/base/crm/skills/x.md"
        )));
        assert!(!is_signed_pack_page(Path::new("markdown/skills/x.md")));
        assert!(!is_signed_pack_page(Path::new("base/x.md")));
    }
}
