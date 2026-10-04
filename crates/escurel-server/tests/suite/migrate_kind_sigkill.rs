//! `escurel admin migrate-kind --apply` killed with a LITERAL `kill -9`, at many points, against the
//! real `escurel-server` process over a real on-disk store with thousands of legacy pages.
//!
//! The crash tests in `escurel-index` interrupt the migration with a failing store; this one stops
//! the whole process the way an OOM killer or a node loss does, and then boots a NEW process over
//! the same directory. The properties pinned (docs/deploy/kind-migration.md):
//!
//! * a tenant is never served silently while its index is incomplete: either `/readyz` says
//!   `quarantined` / `migration_pending` (and tools answer `tenant_quarantined`), or what it serves
//!   is the COMPLETE corpus;
//! * a second `--apply` always completes, whatever point the first one died at;
//! * the end state is exact: every page is `kind:`, the user's own `type: reseller` data field and a
//!   prose `type: instance` mention are untouched, the marker is gone, the tenant serves all pages;
//! * a third `--apply` has nothing left to migrate.
//!
//! The kill moment is chosen by watching for the durable marker file the migration writes before its
//! first rewrite, then waiting a configurable number of milliseconds, so the ten rounds land in
//! different phases (pages, drafts/snapshots, rebuild, after-rebuild). Only the kill TIMING varies
//! between rounds; every assertion is deterministic.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "default";
const SKILLS: usize = 5;
/// Enough pages that the rewrite + rebuild window is hundreds of milliseconds wide.
const INSTANCES: usize = 2_000;
const USER_DATA_PAGE: &str = "markdown/instances/s0/reseller-page.md";
const MARKER: &str = "meta/migrate-kind.pending";
/// Where in the migration the SIGKILL lands. Keyed to PHASES (what is on disk), not to wall-clock
/// guesses, so the rounds hit the same windows on a fast laptop and on a loaded CI runner.
#[derive(Clone, Copy, Debug)]
enum KillPoint {
    /// The durable marker exists; `ms` later (the first rewrites are in flight).
    AfterMarker { ms: u64 },
    /// `percent` of the legacy pages have been rewritten to `kind:` in the lane.
    PagesRewritten { percent: usize },
    /// Every page is rewritten (drafts, snapshots and the full index REBUILD still ahead); `ms` later.
    LaneComplete { ms: u64 },
    /// The migration finished: marker cleared, quarantine lifted.
    Finished,
}

/// The default sweep: one point per phase. `ESCUREL_SIGKILL_FULL=1` runs all ten (a rebuild of 2,000
/// pages takes a minute on a busy machine, so the default keeps CI bounded).
const DEFAULT_POINTS: [KillPoint; 4] = [
    KillPoint::AfterMarker { ms: 0 },
    KillPoint::PagesRewritten { percent: 50 },
    KillPoint::LaneComplete { ms: 0 },
    KillPoint::LaneComplete { ms: 4_000 },
];
const FULL_POINTS: [KillPoint; 10] = [
    KillPoint::AfterMarker { ms: 0 },
    KillPoint::AfterMarker { ms: 6 },
    KillPoint::PagesRewritten { percent: 25 },
    KillPoint::PagesRewritten { percent: 50 },
    KillPoint::PagesRewritten { percent: 75 },
    KillPoint::LaneComplete { ms: 0 },
    KillPoint::LaneComplete { ms: 1_000 },
    KillPoint::LaneComplete { ms: 5_000 },
    KillPoint::LaneComplete { ms: 15_000 },
    KillPoint::Finished,
];

fn lane_root(data_dir: &Path) -> PathBuf {
    data_dir.join("tenants").join(TENANT)
}

/// Write a LEGACY lane: `type:` skills and instances, plus one instance that carries a user data
/// field literally named `type` and prose that says `type: instance`.
fn seed_legacy_lane(data_dir: &Path) {
    let root = lane_root(data_dir);
    for s in 0..SKILLS {
        let dir = root.join("markdown/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("s{s}.md")),
            format!("---\ntype: skill\nid: s{s}\ndescription: skill {s}\n---\n# s{s}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join(format!("markdown/instances/s{s}"))).unwrap();
    }
    for i in 0..INSTANCES {
        let s = i % SKILLS;
        std::fs::write(
            root.join(format!("markdown/instances/s{s}/i{i}.md")),
            format!(
                "---\ntype: instance\nskill: s{s}\nid: i{i}\n---\n# Instance {i}\n\nBody {i}.\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        root.join(USER_DATA_PAGE),
        user_data_page_legacy_with_data_field(),
    )
    .unwrap();
}

/// A legacy page that also carries the user's own data field (`reseller_type`) and prose that says
/// `type: instance`: the migration must rewrite the page kind and nothing else.
fn user_data_page_legacy_with_data_field() -> &'static str {
    "---\ntype: instance\nskill: s0\nid: reseller-page\nreseller_type: gold\n---\n\
     # Reseller\n\nA prose line that says type: instance must stay.\n"
}

// The child is handed back to the caller, which reaps it in `sigkill` / `sigterm`; clippy cannot see
// across the return.
#[allow(clippy::zombie_processes)]
fn spawn_server(data_dir: &Path) -> (Child, String) {
    use assert_cmd::cargo::CommandCargoExt as _;
    let mut child = Command::cargo_bin("escurel-server")
        .expect("locate escurel-server")
        .env("ESCUREL_SERVER_DATA_DIR", data_dir)
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("ESCUREL_EMBEDDING_PROVIDER", "zero")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn escurel-server");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).expect("read server stdout");
        if n == 0 {
            let status = child.wait().expect("reap the exited server");
            panic!("escurel-server exited before it listened ({status})");
        }
        if let Some(rest) = line.trim().strip_prefix("escurel-server listening http=") {
            // Keep draining stdout so the child never blocks on a full pipe.
            std::thread::spawn(move || {
                let mut sink = String::new();
                while reader.read_line(&mut sink).map(|n| n > 0).unwrap_or(false) {
                    sink.clear();
                }
            });
            return (child, format!("http://{rest}"));
        }
    }
}

fn sigkill(mut child: Child) {
    child.kill().expect("SIGKILL the server"); // std's kill is SIGKILL on unix
    let _ = child.wait();
}

fn sigterm(mut child: Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
}

async fn tool(base: &str, name: &str, args: Value) -> Result<Value, String> {
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("transport: {e}"))?;
    let body: Value = resp.json().await.map_err(|e| format!("body: {e}"))?;
    if let Some(err) = body.get("error") {
        return Err(err.to_string());
    }
    Ok(body["result"]["structuredContent"].clone())
}

/// Search a JSON value for a boolean field, wherever the probe nests it.
fn flag(v: &Value, name: &str) -> bool {
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, x)| (k == name && x == &Value::Bool(true)) || flag(x, name)),
        Value::Array(a) => a.iter().any(|x| flag(x, name)),
        _ => false,
    }
}

async fn readyz(base: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("GET /readyz");
    assert_eq!(
        resp.status(),
        200,
        "/readyz must stay 200 (the migration needs the server up)"
    );
    resp.json().await.expect("readyz json")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `(pages still on the removed `type:` key, pages on `kind:`)` read straight off the lane files.
fn lane_kinds(data_dir: &Path) -> (usize, usize) {
    let mut files = Vec::new();
    walk(&lane_root(data_dir).join("markdown"), &mut files);
    let (mut legacy, mut kind) = (0, 0);
    for f in files {
        // The server seeds its own meta skill into every tenant; it is not part of the corpus.
        if f.ends_with("markdown/skills/escurel.md") {
            continue;
        }
        // The store writes through a temp file + rename, so a file listed a moment ago can be gone by
        // now while the migration is running: that is a page mid-rewrite, not a failure.
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let head = text.split("\n---").next().unwrap_or("");
        if head
            .lines()
            .any(|l| l == "type: skill" || l == "type: instance")
        {
            legacy += 1;
        }
        if head
            .lines()
            .any(|l| l == "kind: skill" || l == "kind: instance")
        {
            kind += 1;
        }
    }
    (legacy, kind)
}

async fn served_counts(base: &str) -> Result<(usize, usize), String> {
    let skills = tool(base, "list_skills", json!({})).await?;
    let skill_ids: Vec<String> = skills["skills"]
        .as_array()
        .ok_or("list_skills returned no skills array")?
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_owned))
        .filter(|id| id.starts_with('s') && id[1..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut total = 0;
    for id in &skill_ids {
        let mut cursor: Option<String> = None;
        loop {
            let mut args = json!({ "skill_id": id, "limit": 1000 });
            if let Some(c) = &cursor {
                args["cursor"] = json!(c);
            }
            let page = tool(base, "list_instances", args).await?;
            total += page["instances"].as_array().map_or(0, Vec::len);
            match page["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_owned()),
                None => break,
            }
        }
    }
    Ok((skill_ids.len(), total))
}

/// One round: legacy store -> boot -> `--apply` -> SIGKILL at a chosen `KillPoint` ->
/// reboot -> invariants -> second `--apply` -> exact end state -> third `--apply` is a no-op.
async fn round(point: KillPoint) {
    let started = Instant::now();
    let lap = |what: &str| {
        eprintln!(
            "[{point:?}] {:>6.1}s {what}",
            started.elapsed().as_secs_f32()
        )
    };
    let data = TempDir::new().unwrap();
    seed_legacy_lane(data.path());
    let marker = lane_root(data.path()).join(MARKER);

    // 1. boot over the legacy lane: quarantined, serving nothing.
    let (server, base) = spawn_server(data.path());
    assert!(
        flag(&readyz(&base).await, "quarantined"),
        "a legacy lane must boot quarantined"
    );

    // 2. start the migration and kill the process a chosen moment after it announced itself.
    let apply_base = base.clone();
    let apply =
        tokio::spawn(
            async move { tool(&apply_base, "migrate_kind", json!({ "apply": true })).await },
        );
    let waited = Instant::now();
    let total_pages = SKILLS + INSTANCES + 1;
    loop {
        assert!(
            waited.elapsed() < Duration::from_secs(300),
            "never reached {point:?}"
        );
        let started = marker.exists();
        let done = apply.is_finished();
        let reached = match point {
            KillPoint::AfterMarker { .. } => started || done,
            KillPoint::PagesRewritten { percent } => {
                lane_kinds(data.path()).1 * 100 >= total_pages * percent || done
            }
            KillPoint::LaneComplete { .. } => lane_kinds(data.path()).0 == 0 || done,
            KillPoint::Finished => done,
        };
        if reached {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    match point {
        KillPoint::AfterMarker { ms } | KillPoint::LaneComplete { ms } => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        _ => {}
    }
    sigkill(server);
    lap("killed");
    let first = apply.await.expect("apply task");
    let finished_before_kill = first.is_ok();

    // 3. a NEW process over the same directory.
    let (server, base) = spawn_server(data.path());
    lap("rebooted");
    let ready = readyz(&base).await;
    let (quarantined, pending) = (
        flag(&ready, "quarantined"),
        flag(&ready, "migration_pending"),
    );
    let (legacy_on_disk, _) = lane_kinds(data.path());
    if !quarantined && !pending {
        // Nothing flagged => it must be the complete, migrated corpus.
        assert_eq!(
            legacy_on_disk, 0,
            "served as healthy while legacy pages remain ({point:?})"
        );
        assert!(
            !marker.exists(),
            "healthy but the marker is still there ({point:?})"
        );
        let counts = served_counts(&base).await.expect("a healthy tenant serves");
        assert_eq!(
            counts,
            (SKILLS, INSTANCES + 1),
            "served an INCOMPLETE index with no flag ({point:?}, apply finished: {finished_before_kill})"
        );
    } else {
        // Flagged: tools must refuse rather than serve a half-built index (or, once the rebuild
        // already ran, serve it complete).
        match served_counts(&base).await {
            Err(e) => assert!(e.contains("tenant_quarantined"), "unexpected refusal: {e}"),
            Ok(counts) => assert_eq!(
                counts,
                (SKILLS, INSTANCES + 1),
                "a flagged tenant served a partial corpus ({point:?})"
            ),
        }
    }

    // 4. the second apply always completes, from whatever state the kill left.
    let second = tool(&base, "migrate_kind", json!({ "apply": true }))
        .await
        .unwrap_or_else(|e| panic!("second apply failed ({point:?}): {e}"));
    assert_eq!(
        second["tenant_quarantined"],
        json!(false),
        "second apply left it quarantined: {second}"
    );

    lap("second apply done");
    // 5. exact end state.
    let (legacy, kind) = lane_kinds(data.path());
    assert_eq!(
        legacy, 0,
        "legacy pages remain after the second apply ({point:?})"
    );
    assert_eq!(
        kind,
        SKILLS + INSTANCES + 1,
        "every page is kind: ({point:?})"
    );
    assert!(
        !marker.exists(),
        "the marker outlived a completed migration ({point:?})"
    );
    let ready = readyz(&base).await;
    assert!(
        !flag(&ready, "quarantined") && !flag(&ready, "migration_pending"),
        "{ready}"
    );
    assert_eq!(served_counts(&base).await.unwrap(), (SKILLS, INSTANCES + 1));
    let user_page = std::fs::read_to_string(lane_root(data.path()).join(USER_DATA_PAGE)).unwrap();
    assert!(
        user_page.contains("reseller_type: gold"),
        "user data field changed:\n{user_page}"
    );
    assert!(
        user_page.contains("prose line that says type: instance"),
        "prose changed:\n{user_page}"
    );
    assert!(
        user_page.starts_with("---\nkind: instance\n"),
        "page kind not rewritten:\n{user_page}"
    );

    // 6. idempotent: nothing left to migrate.
    let third = tool(&base, "migrate_kind", json!({ "apply": true }))
        .await
        .unwrap();
    assert_eq!(
        third["pages_to_migrate"],
        json!([]),
        "third apply found work: {third}"
    );
    lap("done");
    sigterm(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sigkill_at_any_point_of_migrate_kind_never_leaves_a_silently_incomplete_tenant() {
    let full = std::env::var("ESCUREL_SIGKILL_FULL").is_ok_and(|v| v == "1");
    let points: &[KillPoint] = if full { &FULL_POINTS } else { &DEFAULT_POINTS };
    for point in points {
        round(*point).await;
    }
}
