//! `escurel-test-gateway` — a real gateway that verifies tokens, for harnesses that are not
//! Rust.
//!
//! Starts the same in-process gateway and OIDC issuer the Rust suites use, seeds it from a
//! directory, prints ONE line of JSON on stdout — the connection details — and then stays up
//! until it is signalled. See `tests/test_gateway_bin.rs` for why this exists.
//!
//! ```text
//! escurel-test-gateway --tenant vsx --seed test/integration/seed [--subject alice]
//!                       [--bearer-file <path> [--bearer-refresh-secs 240]]
//! {"gateway_url":"http://127.0.0.1:…","issuer_url":"http://127.0.0.1:…","kid":"…",
//!  "signing_key":"-----BEGIN RSA PRIVATE KEY-----…","bearer":"eyJ…","admin_bearer":"eyJ…","tenant":"vsx"}
//! ```
//!
//! The seed directory holds `skills/*.md` and `instances/*.md`. Each file becomes the page
//! `markdown/skills/<name>.md` / `markdown/instances/<name>.md` — FLAT, so an instance is
//! `markdown/instances/<skill>__<id>.md`, which is how the shipped corpora lay them out. One level of
//! subdirectory is NESTED: `instances/<skill>/<id>.md` is `markdown/instances/<skill>/<id>.md` (the page
//! id of a row of an `instances: rows` skill, and of its linked markdown).
//!
//! The `bearer` is a HUMAN's (role `agent`, the subject given by `--subject`): it can read,
//! draft and promote, and it is what an editor under test signs in with. The `admin_bearer` is
//! the same subject with the admin role, for what only an admin may do (requeue, pause and
//! resume the runner). Both expire in ten minutes, like every token this issuer mints; that is
//! plenty for a test run and is not extended here, because a long-lived credential printed to
//! stdout is the thing to avoid.
//!
//! The gateway also holds a signing identity on the issuer's own key, so `mint_agent_token` works.
//!
//! A demo outlasts that. `--bearer-file <path>` writes `{bearer, admin_bearer}` there BEFORE the
//! line is printed and replaces it (by rename, so a reader never sees half a file) with fresh
//! ones every `--bearer-refresh-secs` (default 240). Nothing else is ever written.

use std::path::{Path, PathBuf};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

struct Args {
    tenant: String,
    seed: PathBuf,
    subject: String,
    /// Keep this file holding a current `{bearer, admin_bearer}` (a demo outlasts a token).
    bearer_file: Option<PathBuf>,
    bearer_refresh_secs: u64,
}

fn parse() -> Result<Args, String> {
    let mut tenant = None;
    let mut seed = None;
    let mut subject = "alice".to_owned();
    let mut bearer_file = None;
    let mut bearer_refresh_secs = 240;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = |it: &mut dyn Iterator<Item = String>| {
            it.next().ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--tenant" => tenant = Some(value(&mut it)?),
            "--seed" => seed = Some(PathBuf::from(value(&mut it)?)),
            "--subject" => subject = value(&mut it)?,
            "--bearer-file" => bearer_file = Some(PathBuf::from(value(&mut it)?)),
            "--bearer-refresh-secs" => {
                bearer_refresh_secs = value(&mut it)?
                    .parse()
                    .map_err(|_| "--bearer-refresh-secs is a whole number of seconds".to_owned())?;
            }
            "-h" | "--help" => {
                return Err(
                    "usage: escurel-test-gateway --tenant <id> --seed <dir> [--subject <sub>]"
                        .into(),
                );
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Args {
        tenant: tenant.ok_or("--tenant is required")?,
        seed: seed.ok_or("--seed is required")?,
        subject,
        bearer_file,
        bearer_refresh_secs: bearer_refresh_secs.max(1),
    })
}

/// Replace `path` with `contents` so a reader sees the old file or the new one, never half of
/// either: write a sibling, then rename it over.
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// A fresh `{bearer, admin_bearer}` pair for the signed-in subject, written atomically.
fn write_bearers(process: &EscurelProcess, args: &Args, path: &Path) -> std::io::Result<()> {
    let contents = json!({
        "bearer": process.mint_token_with_sub(&args.tenant, Role::Agent, &args.subject),
        "admin_bearer": process.mint_token_with_sub(&args.tenant, Role::Admin, &args.subject),
    })
    .to_string();
    write_atomically(path, &contents)
}

/// `skills/` and `instances/` markdown files, in a stable order so a seed replays the same way
/// every time (skills first, so an instance's skill exists when it is written).
fn pages(seed: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for dir in ["skills", "instances"] {
        let root = seed.join(dir);
        if !root.is_dir() {
            continue;
        }
        for (rel, path) in markdown_files(&root)? {
            let body = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            out.push((format!("markdown/{dir}/{rel}"), body));
        }
    }
    if out.is_empty() {
        return Err(format!(
            "{} holds no skills/ or instances/ markdown",
            seed.display()
        ));
    }
    Ok(out)
}

/// The `.md` files directly under `root` (a flat page id) and one level down (`<skill>/<id>.md`, the
/// NESTED page id an `instances: rows` skill gives a row and its linked markdown), sorted so a seed
/// replays the same way every time. Returns `(relative path, absolute path)`.
fn markdown_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut found = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("read {}: {e}", root.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("non-utf8 file name {}", path.display()))?
            .to_owned();
        if path.is_dir() {
            let mut inner: Vec<PathBuf> = std::fs::read_dir(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
                .collect();
            inner.sort();
            for file in inner {
                let leaf = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| format!("non-utf8 file name {}", file.display()))?;
                found.push((format!("{name}/{leaf}"), file));
            }
        } else if path.extension().is_some_and(|x| x == "md") {
            found.push((name, path));
        }
    }
    Ok(found)
}

#[tokio::main]
async fn main() {
    // FIRST, before anything slow: a parent that terminates this process the moment it has read
    // the connection line (or earlier) must get a clean exit, not a process killed by the
    // default SIGTERM action in the window before a handler exists. A signal that arrives while
    // the gateway is still starting is remembered and ends the wait below at once.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");

    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let pages = match pages(&args.seed) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let mut tenant = FixtureBuilder::new().tenant(&args.tenant);
    for (path, body) in pages {
        tenant = tenant.page(&path, body);
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(tenant.done()),
        config_overrides: ConfigOverrides {
            // Live CRDT sessions, so the editor's personal-draft path works against it.
            live_crdt: true,
            // A signing identity on the issuer's own key, so `mint_agent_token` works: starting
            // a skill in a terminal under a governed run needs it.
            signing: true,
            ..Default::default()
        },
    })
    .await;

    let (signing_key, kid) = process.signing_material();
    let bearer = process.mint_token_with_sub(&args.tenant, Role::Agent, &args.subject);
    let admin_bearer = process.mint_token_with_sub(&args.tenant, Role::Admin, &args.subject);
    // A demo outlasts a ten-minute token. The file is written BEFORE the line is printed, so a
    // reader that waits for the line always finds a bearer, and then kept fresh. Only what the
    // flag asked for is written, and only to the path given.
    if let Some(path) = &args.bearer_file
        && let Err(e) = write_bearers(&process, &args, path)
    {
        eprintln!("cannot write {}: {e}", path.display());
        std::process::exit(2);
    }
    // One line, flushed: the parent reads exactly this and nothing else from stdout.
    println!(
        "{}",
        json!({
            "gateway_url": process.base_url(),
            "issuer_url": process.issuer_url(),
            "kid": kid,
            "signing_key": signing_key,
            "bearer": bearer,
            "admin_bearer": admin_bearer,
            "tenant": args.tenant,
        })
    );

    // Stay up until signalled, then shut the gateway down cleanly rather than leave its
    // data directory and ports behind.
    let rotate = async {
        let Some(path) = &args.bearer_file else {
            return std::future::pending::<()>().await;
        };
        let mut tick =
            tokio::time::interval(std::time::Duration::from_secs(args.bearer_refresh_secs));
        tick.tick().await; // the first tick is immediate; the file was written above
        loop {
            tick.tick().await;
            if let Err(e) = write_bearers(&process, &args, path) {
                eprintln!("cannot refresh {}: {e}", path.display());
            }
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
        () = rotate => {}
    }
    process.shutdown().await;
}
