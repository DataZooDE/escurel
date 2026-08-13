//! `escurel-server` — the deployable single-binary gateway.
//!
//! 12-factor entry point (CLAUDE.md principle 3): read the
//! `ESCUREL_*` config surface from the environment (over an optional
//! TOML base at `$ESCUREL_CONFIG`), build the real backends, bind the
//! HTTP (`8080`) listener, and run until `SIGTERM` /
//! `SIGINT`. JSON structured logs go to stdout via
//! `escurel_obs::init_telemetry` (installed inside `serve`).
//!
//! Exit codes: `0` on clean shutdown; `1` on a fatal config / wiring
//! error before the server is up.

use std::path::Path;

use escurel_server::{EscurelConfig, selfpack};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Telemetry may or may not be installed yet (a config
            // error can happen before `serve` installs the subscriber),
            // so log to stderr unconditionally as well as via tracing.
            eprintln!("escurel-server: fatal: {e}");
            tracing::error!(error = %e, "escurel-server failed to start");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Self-packaging subcommands (ADR-0011). `pack` folds a markdown
    // corpus into a copy of this binary; `info` / `unpack` introspect the
    // bundle a bundled binary carries. Any other first arg (or none) falls
    // through to the server.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pack") => return cmd_pack(&args[2..]),
        Some("info") => return cmd_info(),
        Some("unpack") => return cmd_unpack(&args[2..]),
        _ => {}
    }

    let mut config = EscurelConfig::from_env()?;
    // Self-packaging seed (ADR-0011): if this binary carries an embedded
    // corpus and no explicit `ESCUREL_SEED_DIR` was given, extract it and
    // seed the tenant from it at boot (the existing, idempotent seed path).
    // Explicit seed dir wins (explicit over implicit).
    if config.seed_dir.is_none()
        && let Some(bundle) = selfpack::bundle_in_current_exe()?
    {
        let dir = config.data_dir.join(".embedded-corpus");
        selfpack::unpack(&bundle, &dir)?;
        let n = selfpack::list_bundle(&bundle).map(|e| e.len()).unwrap_or(0);
        eprintln!("escurel-server: seeding tenant from embedded corpus ({n} file(s))");
        config.seed_dir = Some(dir);
    }

    // `build` installs telemetry inside `serve`, so the first
    // structured log line is emitted from there. Surface the bound
    // addresses for operator visibility once we're up.
    let booted = config.build().await?;
    let handle = booted.handle;
    let refresh_handle = booted.refresh_handle;
    let publish_handle = booted.publish_handle;
    // The single-writer lease (#371) must outlive every write this
    // process serves; it drops (releasing the catalog advisory lock for
    // a successor) only after the drain below.
    let _writer_lease = booted.writer_lease;

    tracing::info!(
        http = %handle.local_addr,
        metrics = ?handle.metrics_addr,
        version = %config.version,
        env = %config.env,
        tenant = %config.tenant,
        embedder_loaded = booted.embedder.is_loaded(),
        "escurel-server up"
    );
    // Also print to stdout so a bare `escurel-server` run (or a test
    // spawning the binary) can observe the bound HTTP address without
    // a tracing subscriber configured for the caller.
    println!("escurel-server listening http={}", handle.local_addr);

    wait_for_shutdown().await;

    tracing::info!("escurel-server received shutdown signal; draining");
    // A ducklake reader's RefreshTask must stop alongside the HTTP/metrics
    // listeners — otherwise the poll loop outlives everything else on a
    // graceful stop.
    if let Some(refresh) = refresh_handle {
        refresh.shutdown().await;
    }
    // A ducklake writer's optional periodic PublishTask (PR 7) must stop
    // alongside everything else, same reasoning as the reader's
    // RefreshTask above.
    if let Some(publish) = publish_handle {
        publish.shutdown().await;
    }
    handle.shutdown().await;
    Ok(())
}

/// `--flag value` lookup in a raw argv slice.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `escurel-server pack --in <dir> --out <bin> [--allow-secrets]` — fold a
/// markdown corpus into a copy of this binary (ADR-0011).
fn cmd_pack(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let in_dir = flag(args, "--in").ok_or("pack: --in <dir> is required")?;
    let out = flag(args, "--out").ok_or("pack: --out <file> is required")?;
    let allow_secrets = args.iter().any(|a| a == "--allow-secrets");

    let exe = std::fs::read(std::env::current_exe()?)?;
    let base = selfpack::base_image(&exe);
    let bundle = selfpack::build_bundle(Path::new(&in_dir), allow_secrets)?;
    let out_bytes = selfpack::append_bundle(base, &bundle);
    std::fs::write(&out, &out_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))?;
    }
    let n = selfpack::list_bundle(&bundle).map(|e| e.len()).unwrap_or(0);
    println!(
        "packed {n} file(s) from {in_dir} into {out} ({} bytes)",
        out_bytes.len()
    );
    Ok(())
}

/// `escurel-server info` — report the bundle this binary carries.
fn cmd_info() -> Result<(), Box<dyn std::error::Error>> {
    match selfpack::bundle_in_current_exe()? {
        Some(bundle) => {
            let entries = selfpack::list_bundle(&bundle)?;
            println!("bundle: {} file(s)", entries.len());
            for (path, size) in entries {
                println!("  {path} ({size} bytes)");
            }
        }
        None => println!("no bundle: this is an unpacked escurel-server"),
    }
    Ok(())
}

/// `escurel-server unpack --to <dir>` — extract this binary's bundle.
fn cmd_unpack(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let to = flag(args, "--to").ok_or("unpack: --to <dir> is required")?;
    match selfpack::bundle_in_current_exe()? {
        Some(bundle) => {
            selfpack::unpack(&bundle, Path::new(&to))?;
            println!("unpacked bundle to {to}");
            Ok(())
        }
        None => Err("no bundle to unpack (this is an unpacked escurel-server)".into()),
    }
}

/// Block until SIGTERM (the orchestrator's graceful-stop signal) or SIGINT
/// (Ctrl-C in a dev shell). On non-unix targets, only Ctrl-C.
#[cfg(unix)]
async fn wait_for_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
