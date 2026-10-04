//! The `ESCUREL_*` surface is documented in ONE place, generated from the code, and checked both
//! ways. Round-2 review: `--help` -> deploy README -> `config.rs` -> spec was a circle of hand-kept
//! tables; ~40 real variables (`ESCUREL_RUNNER_*`, `ESCUREL_RETRIEVAL_*`, ...) were missing from every
//! one of them and the spec listed five variables that do not exist.
//!
//! Pinned here:
//!  * every `ESCUREL_*` name the sources READ is in the registry (`config_keys::CONFIG_KEYS`);
//!  * every name in the registry is still read somewhere (no phantoms);
//!  * `docs/deploy/env.md` is exactly what `--print-config-keys` prints (not stale);
//!  * the operator documents name no variable that does not exist.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use escurel_server::config_keys::{CONFIG_KEYS, render_markdown};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn walk(dir: &Path, ext: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(
                name,
                "target" | "node_modules" | ".git" | "build" | "dist" | ".dart_tool"
            ) {
                continue;
            }
            walk(&p, ext, out);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| ext.contains(&x))
            || ext.contains(&name)
        {
            out.push(p);
        }
    }
}

/// `ESCUREL_[A-Z0-9_]*` tokens of a text.
fn tokens(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(off) = text[i..].find("ESCUREL_") {
        let start = i + off;
        // A token inside a longer identifier (`NOT_ESCUREL_X`, `X_ESCUREL_`) is not ours.
        let before_ok =
            start == 0 || !(b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_');
        let mut end = start;
        while end < b.len()
            && (b[end].is_ascii_uppercase() || b[end].is_ascii_digit() || b[end] == b'_')
        {
            end += 1;
        }
        if before_ok {
            out.insert(text[start..end].to_owned());
        }
        i = end.max(start + 1);
    }
    out
}

/// Is `t` covered by the registry (exact, a numbered/named family, or a bare family prefix)?
fn known(t: &str) -> bool {
    CONFIG_KEYS.iter().any(|k| {
        let n = k.name;
        if n == t {
            return true;
        }
        // `FOO_<N>` / `FOO_<NAME>` family: `FOO_2`, `FOO_CRM`, and the bare prefix `FOO_`.
        if let Some(prefix) = n.split_once('<').map(|(p, _)| p)
            && t.starts_with(prefix)
        {
            return true;
        }
        // A prefix written in code or prose (`ESCUREL_EGRESS_`, `ESCUREL_RUNNER_`).
        t.ends_with('_') && n.starts_with(t)
    })
}

fn source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = repo().join("crates");
    for c in std::fs::read_dir(crates).unwrap().flatten() {
        walk(&c.path().join("src"), &["rs"], &mut files);
    }
    files
}

#[test]
fn every_variable_the_sources_read_is_in_the_registry() {
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for f in source_files() {
        if f.ends_with("config_keys.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        // Production code only: the inline `#[cfg(test)]` module is where tests invent names.
        let prod = text.split("#[cfg(test)]").next().unwrap_or("");
        for t in tokens(prod) {
            if !known(&t) {
                missing.insert(format!(
                    "{t}  ({})",
                    f.strip_prefix(repo()).unwrap().display()
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "variables read in the sources but absent from `config_keys::CONFIG_KEYS` (add them with a default and a meaning):\n{}",
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn every_registry_entry_is_still_read_somewhere() {
    let mut all = String::new();
    let mut files = source_files();
    for c in std::fs::read_dir(repo().join("crates")).unwrap().flatten() {
        walk(&c.path().join("tests"), &["rs"], &mut files);
    }
    for dir in [
        "scripts",
        "deploy",
        ".github",
        "examples",
        "apps",
        "editors/vscode/test",
        "editors/vscode/demo",
    ] {
        walk(
            &repo().join(dir),
            &[
                "rs", "sh", "yml", "yaml", "mjs", "ts", "dart", "example", "toml",
            ],
            &mut files,
        );
    }
    files.push(repo().join("Dockerfile"));
    for f in files {
        if f.ends_with("config_keys.rs") {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(&f) {
            all.push_str(&t);
        }
    }
    let present = tokens(&all);
    let phantoms: Vec<&str> = CONFIG_KEYS
        .iter()
        .map(|k| k.name)
        // `VERSION` / `ENV` are the substrate's own spellings, not `ESCUREL_*`: not token-scanned.
        .filter(|n| n.starts_with("ESCUREL_"))
        .filter(|n| {
            let probe = n.split_once('<').map_or(*n, |(p, _)| p);
            !present
                .iter()
                .any(|t| t == n || (n.contains('<') && t.starts_with(probe)))
        })
        .collect();
    assert!(
        phantoms.is_empty(),
        "registry entries nothing reads (remove them, or the code that read them was deleted): {phantoms:?}"
    );
}

#[test]
fn the_committed_env_table_is_what_the_registry_renders() {
    let on_disk = std::fs::read_to_string(repo().join("docs/deploy/env.md"))
        .expect("docs/deploy/env.md (generate it with `escurel-server --print-config-keys > docs/deploy/env.md`)");
    assert!(
        on_disk == render_markdown(),
        "docs/deploy/env.md is stale: regenerate it with `escurel-server --print-config-keys > docs/deploy/env.md`"
    );
}

#[test]
fn operator_documents_name_no_variable_that_does_not_exist() {
    let mut files = Vec::new();
    for dir in ["docs/deploy", "docs/spec", "deploy"] {
        walk(
            &repo().join(dir),
            &["md", "yml", "yaml", "example", "sh"],
            &mut files,
        );
    }
    files.push(repo().join("README.md"));
    files.push(repo().join("deploy/compose/.env.example"));
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for t in tokens(&text) {
            if !known(&t) {
                unknown.insert(format!(
                    "{t}  ({})",
                    f.strip_prefix(repo()).unwrap().display()
                ));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "operator documents name variables the binaries do not read:\n{}",
        unknown.into_iter().collect::<Vec<_>>().join("\n")
    );
}
