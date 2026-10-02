//! `escurel-test-gateway` — a real gateway that verifies tokens, for harnesses that are not
//! Rust.
//!
//! Starts the same in-process gateway and OIDC issuer the Rust suites use, seeds it from a
//! directory, prints ONE line of JSON on stdout — the connection details — and then stays up
//! until it is signalled. See `tests/test_gateway_bin.rs` for why this exists.
//!
//! ```text
//! escurel-test-gateway --tenant vsx --seed test/integration/seed [--subject alice]
//! {"gateway_url":"http://127.0.0.1:…","issuer_url":"http://127.0.0.1:…","kid":"…",
//!  "signing_key":"-----BEGIN RSA PRIVATE KEY-----…","bearer":"eyJ…","admin_bearer":"eyJ…","tenant":"vsx"}
//! ```
//!
//! The seed directory holds `skills/*.md` and `instances/*.md`. Each file becomes the page
//! `markdown/skills/<name>.md` / `markdown/instances/<name>.md` — FLAT, so an instance is
//! `markdown/instances/<skill>__<id>.md`, which is how the shipped corpora lay them out.
//!
//! The `bearer` is a HUMAN's (role `agent`, the subject given by `--subject`): it can read,
//! draft and promote, and it is what an editor under test signs in with. The `admin_bearer` is
//! the same subject with the admin role, for what only an admin may do (requeue, pause and
//! resume the runner). Both expire in ten minutes, like every token this issuer mints; that is
//! plenty for a test run and is not extended here, because a long-lived credential printed to
//! stdout is the thing to avoid.

use std::path::{Path, PathBuf};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

struct Args {
    tenant: String,
    seed: PathBuf,
    subject: String,
}

fn parse() -> Result<Args, String> {
    let mut tenant = None;
    let mut seed = None;
    let mut subject = "alice".to_owned();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = |it: &mut dyn Iterator<Item = String>| {
            it.next().ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--tenant" => tenant = Some(value(&mut it)?),
            "--seed" => seed = Some(PathBuf::from(value(&mut it)?)),
            "--subject" => subject = value(&mut it)?,
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
    })
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
        let mut files: Vec<_> = std::fs::read_dir(&root)
            .map_err(|e| format!("read {}: {e}", root.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        for path in files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("non-utf8 file name {}", path.display()))?;
            let body = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            out.push((format!("markdown/{dir}/{name}"), body));
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

#[tokio::main]
async fn main() {
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
            ..Default::default()
        },
    })
    .await;

    let (signing_key, kid) = process.signing_material();
    let bearer = process.mint_token_with_sub(&args.tenant, Role::Agent, &args.subject);
    let admin_bearer = process.mint_token_with_sub(&args.tenant, Role::Admin, &args.subject);
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
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    process.shutdown().await;
}
