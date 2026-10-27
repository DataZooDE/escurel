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
//!                       [--group <name>]... [--write-acl-enforce] [--event-acl-enforce]
//!                       [--service-subject <sub> [--service-group <name>]...] [--tenant-export]
//!                       [--external-issuer-url <url> --external-jwks-url <url>]
//! {"gateway_url":"http://127.0.0.1:…","issuer_url":"http://127.0.0.1:…","kid":"…",
//!  "signing_key":"-----BEGIN RSA PRIVATE KEY-----…","bearer":"eyJ…","admin_bearer":"eyJ…","operator_bearer":"eyJ…","tenant":"vsx"}
//! ```
//! Repeating `--group` sets the groups in locally minted bearer tokens. With no `--group`, the
//! bearer has `escurel:agent`. External issuer mode returns null for local signing material and
//! bearers; the caller supplies its own token.
//!
//! The seed directory holds `skills/*.md` and `instances/*.md`. Each file becomes the page
//! `markdown/skills/<name>.md` / `markdown/instances/<name>.md` — FLAT, so an instance is
//! `markdown/instances/<skill>__<id>.md`, which is how the shipped corpora lay them out. One level of
//! subdirectory is NESTED: `instances/<skill>/<id>.md` is `markdown/instances/<skill>/<id>.md` (the page
//! id of a row of an `instances: rows` skill, and of its linked markdown).
//!
//! The locally minted `bearer` is a HUMAN's (role `agent`, the subject given by `--subject`): it can read,
//! draft and promote, and it is what an editor under test signs in with. The `admin_bearer` is
//! the same subject with the admin role, for what only an admin may do (requeue, pause and
//! resume the runner). Both expire in ten minutes, like every token this issuer mints; that is
//! plenty for a test run and is not extended here, because a long-lived credential printed to
//! stdout is the thing to avoid.
//!
//! The `operator_bearer` carries the operator role beside admin: the only credential that may name
//! another tenant in the cross-tenant admin tools (tenant registry, `tenant_export`/`tenant_import`).
//! Its `tenant` claim is still the served tenant.
//!
//! The gateway also holds a signing identity on the issuer's own key, so `mint_agent_token` works.
//!
//! Production enforcement can be reproduced: `--event-acl-enforce` (or `ESCUREL_EVENT_ACL`) sets
//! the event ACL, `ESCUREL_AUTH_ROLE_TOOLS` configures the service-role gate, and
//! `--service-subject` with `--service-group` adds a `service_bearer` for a worker identity
//! (role `agent`, those groups; also kept fresh in `--bearer-file`).
//!
//! A demo outlasts that. `--bearer-file <path>` writes `{bearer, admin_bearer, operator_bearer}` there BEFORE the
//! line is printed and replaces it (by rename, so a reader never sees half a file) with fresh
//! ones every `--bearer-refresh-secs` (default 240). Nothing else is ever written.

use std::path::{Path, PathBuf};

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
    WriteAclMode,
};
use serde_json::json;

struct Args {
    tenant: String,
    seed: PathBuf,
    subject: String,
    /// Keep this file holding a current `{bearer, admin_bearer, operator_bearer}` (a demo outlasts a token).
    bearer_file: Option<PathBuf>,
    bearer_refresh_secs: u64,
    groups: Vec<String>,
    external_issuer_url: Option<String>,
    external_jwks_url: Option<String>,
    write_acl_enforce: bool,
    event_acl_enforce: bool,
    service_subject: Option<String>,
    service_groups: Vec<String>,
    tenant_export: bool,
}

fn parse() -> Result<Args, String> {
    let mut tenant = None;
    let mut seed = None;
    let mut subject = "alice".to_owned();
    let mut bearer_file = None;
    let mut bearer_refresh_secs = 240;
    let mut groups = Vec::new();
    let mut external_issuer_url = None;
    let mut external_jwks_url = None;
    let mut write_acl_enforce = false;
    let mut event_acl_enforce = false;
    let mut service_subject = None;
    let mut service_groups = Vec::new();
    let mut tenant_export = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = |it: &mut dyn Iterator<Item = String>| {
            it.next().ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--tenant" => tenant = Some(value(&mut it)?),
            "--seed" => seed = Some(PathBuf::from(value(&mut it)?)),
            "--subject" => subject = value(&mut it)?,
            "--group" => groups.push(value(&mut it)?),
            "--bearer-file" => bearer_file = Some(PathBuf::from(value(&mut it)?)),
            "--bearer-refresh-secs" => {
                bearer_refresh_secs = value(&mut it)?
                    .parse()
                    .map_err(|_| "--bearer-refresh-secs is a whole number of seconds".to_owned())?;
            }
            "--external-issuer-url" => external_issuer_url = Some(value(&mut it)?),
            "--external-jwks-url" => external_jwks_url = Some(value(&mut it)?),
            "--write-acl-enforce" => write_acl_enforce = true,
            "--event-acl-enforce" => event_acl_enforce = true,
            "--service-subject" => service_subject = Some(value(&mut it)?),
            "--service-group" => service_groups.push(value(&mut it)?),
            "--tenant-export" => tenant_export = true,
            "-h" | "--help" => {
                return Err(
                    "usage: escurel-test-gateway --tenant <id> --seed <dir> [--subject <sub>] [--group <name>]... [--write-acl-enforce] [--event-acl-enforce] [--service-subject <sub> [--service-group <name>]...] [--tenant-export] [--external-issuer-url <url> --external-jwks-url <url>]"
                        .into(),
                );
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if external_issuer_url.is_some() != external_jwks_url.is_some() {
        return Err("external issuer URL and JWKS URL must be supplied together".into());
    }
    if external_issuer_url.is_some() && bearer_file.is_some() {
        return Err("external issuer mode does not mint or refresh bearer files".into());
    }
    if service_subject.is_none() && !service_groups.is_empty() {
        return Err("--service-group needs --service-subject".into());
    }
    if external_issuer_url.is_some() && service_subject.is_some() {
        return Err("external issuer mode does not mint a service bearer".into());
    }
    if groups.is_empty() {
        groups.push("escurel:agent".to_owned());
    }
    Ok(Args {
        tenant: tenant.ok_or("--tenant is required")?,
        seed: seed.ok_or("--seed is required")?,
        subject,
        bearer_file,
        bearer_refresh_secs: bearer_refresh_secs.max(1),
        groups,
        external_issuer_url,
        external_jwks_url,
        write_acl_enforce,
        event_acl_enforce,
        service_subject,
        service_groups,
        tenant_export,
    })
}

/// Replace `path` with `contents` so a reader sees the old file or the new one, never half of
/// either: write a sibling, then rename it over.
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// A fresh `{bearer, admin_bearer, operator_bearer}` set for the signed-in subject, written atomically.
fn write_bearers(process: &EscurelProcess, args: &Args, path: &Path) -> std::io::Result<()> {
    let contents = json!({
        "bearer": mint_bearer(process, args),
        "admin_bearer": process.mint_token_with_sub(&args.tenant, Role::Admin, &args.subject),
        "operator_bearer": process.mint_operator_token(&args.tenant),
        "service_bearer": mint_service_bearer(process, args),
    })
    .to_string();
    write_atomically(path, &contents)
}

fn mint_bearer(process: &EscurelProcess, args: &Args) -> String {
    let groups: Vec<&str> = args.groups.iter().map(String::as_str).collect();
    process.mint_token_with_groups(&args.tenant, &args.subject, &groups, false)
}

/// The worker identity's bearer, when `--service-subject` asked for one.
fn mint_service_bearer(process: &EscurelProcess, args: &Args) -> Option<String> {
    let subject = args.service_subject.as_deref()?;
    let groups: Vec<&str> = args.service_groups.iter().map(String::as_str).collect();
    Some(process.mint_token_with_groups(&args.tenant, subject, &groups, false))
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
    let external = args
        .external_issuer_url
        .as_ref()
        .zip(args.external_jwks_url.as_ref());
    let process = EscurelProcess::spawn(Opts {
        auth: if let Some((issuer_url, jwks_url)) = external {
            AuthMode::External {
                issuer_url: issuer_url.clone(),
                jwks_url: jwks_url.clone(),
            }
        } else {
            AuthMode::TestIssuer
        },
        fixtures: Some(tenant.done()),
        config_overrides: ConfigOverrides {
            write_acl: args.write_acl_enforce.then_some(WriteAclMode::Enforce),
            // The event ACL is off unless asked for, by flag or by the production variable.
            event_acl: Some(if args.event_acl_enforce {
                EventAclMode::Enforce
            } else {
                EventAclMode::from_env()
            }),
            // The service-role gate, from the production variable.
            role_tools: std::env::var("ESCUREL_AUTH_ROLE_TOOLS")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            fixture_tenant_export: args.tenant_export,
            // Live CRDT sessions, so the editor's personal-draft path works against it.
            live_crdt: true,
            // A signing identity on the issuer's own key, so `mint_agent_token` works: starting
            // a skill in a terminal under a governed run needs it.
            signing: external.is_none(),
            // The demo's REST and MCP upstreams are local processes; `ESCUREL_EGRESS_*` (notably
            // `ESCUREL_EGRESS_ALLOW_LOOPBACK=1`) opens loopback for them, strict otherwise.
            egress: Some(escurel_test_support::EgressPolicy::from_env()),
            // Locally-built DuckDB extensions to LOAD (comma-separated absolute paths), as in
            // production: an unsigned one is allowed at open time when any is named. The S2D demo
            // uses it for the optimizer pages.
            index_extensions: std::env::var("ESCUREL_INDEX_EXTENSIONS")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        },
    })
    .await;

    let (signing_key, kid, bearer, admin_bearer, operator_bearer) = if external.is_none() {
        let (key, kid) = process.signing_material();
        (
            Some(key),
            Some(kid),
            Some(mint_bearer(&process, &args)),
            Some(process.mint_token_with_sub(&args.tenant, Role::Admin, &args.subject)),
            Some(process.mint_operator_token(&args.tenant)),
        )
    } else {
        (None, None, None, None, None)
    };
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
            "issuer_url": external.map_or_else(|| process.issuer_url(), |(url, _)| url.clone()),
            "kid": kid,
            "signing_key": signing_key,
            "bearer": bearer,
            "admin_bearer": admin_bearer,
            "operator_bearer": operator_bearer,
            "service_bearer": mint_service_bearer(&process, &args),
            "tenant": args.tenant,
            "fixture_store_dir": args.tenant_export.then(|| process.fixture_store_dir())
                .flatten().map(|path| path.display().to_string()),
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
