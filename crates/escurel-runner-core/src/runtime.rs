//! Distributed runtimes, runner side (#818).
//!
//! Three things a runner does so that several of them can share one tenant:
//!
//! * **register** — [`runtime_page`] is the `runtime` instance it writes on
//!   boot and whenever [`probe_harnesses`] reads different CLI versions;
//!   liveness is its existing status record, never a page write per beat;
//! * **route** — [`RuntimeGate::admit`] skips an inbox event whose
//!   `requires:` (on the event's provenance or its skill page) this runner
//!   cannot satisfy, BEFORE claiming it, so the event stays visible in the
//!   inbox for a runner that can;
//! * **claim** — for an event it can take, the gate holds a `claim_event`
//!   lease (renewed at half-life on later polls), so exactly one runner
//!   starts it. The ledger stays the local idempotency authority.
//!
//! A gateway older than the claim tools answers `-32601`; the gate then
//! admits everything it can route, which is the single-runner behaviour. A
//! claim the gateway did not answer (timeout, transport, `5xx`) is retried on
//! the next poll, never taken. Admission is one gateway call per inbox event,
//! in series; bounded concurrency is a possible follow-up.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use escurel_client::{
    ClaimEventRequest, ClaimEventResponse, Client, Error, ListEventsRequest, ListSkillsRequest,
};
use escurel_types::Event;
use escurel_types::runtime::{RUNTIME_SKILL, Requires, RuntimeHarness, runtime_slug};
use serde_json::{Value, json};

/// How long the skill catalogue's `requires:` are trusted before re-reading.
const SKILL_CACHE_TTL: Duration = Duration::from_secs(10);
/// How long `<cli> --version` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long admission waits on the gateway before leaving the event for the
/// next poll.
const ADMIT_CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// A version line is a label, not a log.
const MAX_VERSION_LEN: usize = 120;

/// The harness names a runner offers: its default first, then the
/// allow-list, without repeats.
#[must_use]
pub fn harness_set(default: &str, allow: &[String]) -> Vec<String> {
    let mut out = vec![default.to_owned()];
    for name in allow {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

/// Probe each `(name, cli)`: the first line of `<cli> --version`, or `None`
/// for an in-process harness (no CLI) and for a probe that fails.
pub async fn probe_harnesses(harnesses: &[(String, Option<String>)]) -> Vec<RuntimeHarness> {
    let mut out = Vec::with_capacity(harnesses.len());
    for (name, cli) in harnesses {
        let version = match cli {
            Some(bin) => probe_version(bin).await,
            None => None,
        };
        out.push(RuntimeHarness {
            name: name.clone(),
            version,
        });
    }
    out
}

async fn probe_version(bin: &str) -> Option<String> {
    let run = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PROBE_TIMEOUT, run).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(MAX_VERSION_LEN).collect())
}

/// The `runtime` instance page a runner registers as. JSON values are valid
/// YAML flow scalars, so every field is written as JSON: no quoting rules to
/// get wrong for a version string or a `site:muc` label.
#[must_use]
pub fn runtime_page(
    runner_id: &str,
    holder: Option<&str>,
    harnesses: &[RuntimeHarness],
    labels: &[String],
    max_concurrent: u64,
) -> String {
    let slug = runtime_slug(runner_id);
    format!(
        "---\nkind: instance\nskill: {RUNTIME_SKILL}\nid: {slug}\nrunner_id: {}\nharnesses: {}\n\
         labels: {}\nmax_concurrent: {max_concurrent}\nclaim_holder: {}\n---\n# runtime {runner_id}\n\n\
         Registered by `escurel-runner`; liveness is its `runner-status:{runner_id}` record \
         (`list_runtimes`).\n",
        json!(runner_id),
        json!(harnesses),
        json!(labels),
        json!(holder),
    )
}

/// What the gate decided for one inbox event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit {
    /// This runner holds the event's lease (or the gateway has no leases).
    Take,
    /// A `requires:` this runner cannot satisfy: left in the inbox, unclaimed.
    Unroutable,
    /// Another runner holds the lease (`holder`).
    Elsewhere(String),
    /// The event is already processed.
    Done,
    /// Not decidable now (the skill catalogue could not be read, the gateway
    /// did not answer a claim in time): left unclaimed, tried again on the
    /// next poll.
    Retry,
}

struct Held {
    mine: bool,
    until: Instant,
}

/// The runner's routing + claim gate, shared by the poller and the webhook.
pub struct RuntimeGate {
    /// The ledger's stable identity, sent as `holder_id`.
    holder_id: String,
    /// `<sub>#<holder_id>`, as the gateway writes the holder.
    holder: Option<String>,
    harnesses: Vec<String>,
    labels: Vec<String>,
    lease: Duration,
    skills: Mutex<Option<(Instant, HashMap<String, Requires>)>>,
    held: Mutex<HashMap<String, Held>>,
    claims_unsupported: AtomicBool,
    told: Mutex<HashSet<String>>,
}

impl RuntimeGate {
    #[must_use]
    pub fn new(
        holder_id: &str,
        subject: Option<&str>,
        harnesses: Vec<String>,
        labels: Vec<String>,
        lease: Duration,
    ) -> Self {
        Self {
            holder_id: holder_id.to_owned(),
            holder: subject.map(|s| format!("{s}#{holder_id}")),
            harnesses,
            labels,
            lease,
            skills: Mutex::new(None),
            held: Mutex::new(HashMap::new()),
            claims_unsupported: AtomicBool::new(false),
            told: Mutex::new(HashSet::new()),
        }
    }

    /// The holder string the gateway writes for this runner's leases.
    #[must_use]
    pub fn holder(&self) -> Option<&str> {
        self.holder.as_deref()
    }

    /// Whether this runner satisfies everything `event` and its skill require.
    #[must_use]
    pub fn routes(&self, event: &Requires) -> bool {
        event.satisfied_by(&self.harnesses, &self.labels)
    }

    /// Route, then claim. Never claims an event it cannot route.
    pub async fn admit(&self, client: &Client, event: &Event) -> Admit {
        // Bookkeeping is never dispatched (the ledger gate drops it): no
        // routing to do and no lease worth a gateway call.
        if event.kind == "system" || event.label_skill.starts_with("escurel:") {
            return Admit::Take;
        }
        let declared = Requires::declared(event.provenance.get("requires"));
        // An unknown catalogue is not "no requirement": routing on it would
        // send a skill that needs `site:muc` to a runner without the label.
        let Some(skill) = self.skill_requires(client, &event.label_skill).await else {
            self.tell_once("\0catalogue-unknown", || {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %event.event_id,
                    "the skill catalogue could not be read yet; inbox work waits for it (logged once)"
                );
            });
            return Admit::Retry;
        };
        let unroutable = declared
            .iter()
            .chain(skill.iter())
            .find(|r| !self.routes(r));
        if let Some(r) = unroutable {
            self.tell_once(&event.event_id, || {
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %event.event_id,
                    label_skill = %event.label_skill,
                    requires = %json!(r),
                    harnesses = ?self.harnesses,
                    labels = ?self.labels,
                    "inbox event requires what this runner does not offer; left unclaimed"
                );
            });
            return Admit::Unroutable;
        }
        self.claim(client, &event.event_id).await
    }

    /// Hold (or keep holding) the lease on `event_id`.
    pub async fn claim(&self, client: &Client, event_id: &str) -> Admit {
        if self.claims_unsupported.load(Ordering::Relaxed) {
            return Admit::Take;
        }
        let now = Instant::now();
        let renew = {
            let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            match held.get(event_id) {
                Some(h) if h.mine && h.until > now + self.lease / 2 => return Admit::Take,
                Some(h) if !h.mine && h.until > now => {
                    return Admit::Elsewhere(String::new());
                }
                Some(h) => h.mine,
                None => false,
            }
        };
        let req = ClaimEventRequest {
            event_id: event_id.to_owned(),
            lease_secs: self.lease.as_secs().max(1),
            holder_id: self.holder_id.clone(),
        };
        let call = async {
            if renew {
                client.renew_claim(req).await
            } else {
                client.claim_event(req).await
            }
        };
        match tokio::time::timeout(ADMIT_CALL_TIMEOUT, call).await {
            Ok(Ok(r)) => self.record(event_id, &r),
            Ok(Err(e)) if permanent_claim_refusal(&e) => {
                let unsupported = matches!(e, Error::JsonRpc { code: -32601, .. });
                self.no_answer(event_id, unsupported, &e)
            }
            Ok(Err(e)) => self.unanswered(event_id, &e),
            Err(_) => self.unanswered(event_id, &"claim_event timed out"),
        }
    }

    /// The gateway did not answer the claim (slow, unreachable, failing):
    /// another runner may hold or be taking the event, so do not start it;
    /// the next poll asks again. The run could not progress without the
    /// gateway anyway.
    fn unanswered(&self, event_id: &str, e: &dyn std::fmt::Display) -> Admit {
        self.tell_once("\0claim-unanswered", || {
            tracing::warn!(
                target: "escurel_runner",
                event_id,
                error = %e,
                "claim_event gave no answer; the event waits for the next poll (logged once)"
            );
        });
        Admit::Retry
    }

    /// A permanent refusal is not a "no": an event the gateway does not store
    /// (a synthetic webhook), an identity it will not let claim, or an image
    /// without the tool (`unsupported`) all fall back to the single-runner
    /// behaviour — the ledger alone decides. Only a lease another runner HOLDS
    /// keeps this runner off an event.
    fn no_answer(&self, event_id: &str, unsupported: bool, e: &dyn std::fmt::Display) -> Admit {
        if unsupported {
            self.claims_unsupported.store(true, Ordering::Relaxed);
        }
        let key = if unsupported {
            "\0claims-unsupported"
        } else {
            "\0claim-failed"
        };
        self.tell_once(key, || {
            tracing::warn!(
                target: "escurel_runner",
                event_id,
                error = %e,
                "claim_event gave no answer; admitting the event as a single runner would \
                 (logged once)"
            );
        });
        Admit::Take
    }

    fn record(&self, event_id: &str, r: &ClaimEventResponse) -> Admit {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if r.processed {
            held.remove(event_id);
            return Admit::Done;
        }
        let until = Instant::now() + remaining(r.expires_at.as_deref());
        held.insert(
            event_id.to_owned(),
            Held {
                mine: r.claimed,
                until,
            },
        );
        if r.claimed {
            Admit::Take
        } else {
            Admit::Elsewhere(r.holder.clone().unwrap_or_default())
        }
    }

    /// Drop bookkeeping for leases long past (events assigned or gone).
    pub fn prune(&self) {
        let horizon = Instant::now()
            .checked_sub(self.lease.saturating_mul(2))
            .unwrap_or_else(Instant::now);
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, h| h.until > horizon);
        let mut told = self.told.lock().unwrap_or_else(|e| e.into_inner());
        if told.len() > 10_000 {
            told.clear();
        }
    }

    /// What `skill` requires: `Some(None)` for nothing, `None` when the
    /// catalogue has never been read (the last one read is used while a
    /// re-read fails).
    async fn skill_requires(&self, client: &Client, skill: &str) -> Option<Option<Requires>> {
        let fresh = {
            let cache = self.skills.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .as_ref()
                .filter(|(at, _)| at.elapsed() < SKILL_CACHE_TTL)
                .map(|(_, m)| m.get(skill).cloned())
        };
        if let Some(found) = fresh {
            return Some(found);
        }
        let listed = tokio::time::timeout(
            ADMIT_CALL_TIMEOUT,
            client.list_skills(ListSkillsRequest::default()),
        )
        .await;
        match listed {
            Ok(Ok(resp)) => {
                let map: HashMap<String, Requires> = resp
                    .skills
                    .into_iter()
                    .filter_map(|s| s.requires.map(|r| (s.id, r)))
                    .collect();
                let found = map.get(skill).cloned();
                *self.skills.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((Instant::now(), map));
                Some(found)
            }
            failed => {
                let e = match failed {
                    Ok(Err(e)) => e.to_string(),
                    _ => "list_skills timed out".to_owned(),
                };
                tracing::warn!(target: "escurel_runner", error = %e, "list_skills failed; routing on the last catalogue");
                let cache = self.skills.lock().unwrap_or_else(|e| e.into_inner());
                cache.as_ref().map(|(_, m)| m.get(skill).cloned())
            }
        }
    }

    fn tell_once(&self, event_id: &str, say: impl FnOnce()) {
        if self
            .told
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(event_id.to_owned())
        {
            say();
        }
    }
}

/// The live claim on the event that started `run_id`, from the run's own
/// `run-started` record and the event's by-id lookup. `None` when the run is
/// unknown or nobody holds its event.
pub async fn run_claim_holder(client: &Client, run_id: &str) -> Option<String> {
    let own = client
        .list_events(ListEventsRequest {
            run_id: run_id.to_owned(),
            ..Default::default()
        })
        .await
        .ok()?;
    let trigger = own.events.iter().find_map(|e| {
        e.provenance["runner"]["event_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })?;
    let looked = client
        .list_events(ListEventsRequest {
            event_id: Some(trigger),
            ..Default::default()
        })
        .await
        .ok()?;
    looked.claim.map(|c| c.holder)
}

/// A claim refusal that no retry changes, after which the runner admits the
/// event as a lone runner would: a gateway without the tool (`-32601`), an
/// identity it will not let claim (`-32001` admin required, `-32003`
/// forbidden, `401`/`403`), or an event it does not store / a malformed
/// claim (`-32602`, e.g. `event_not_found` for a synthetic webhook event).
/// Anything else — a timeout, a transport failure, a `5xx`/`429`, an
/// internal error — may be the gateway being slow, and is retried.
fn permanent_claim_refusal(e: &Error) -> bool {
    match e {
        Error::JsonRpc { code, .. } => matches!(code, -32601 | -32001 | -32003 | -32602),
        Error::Http { status, .. } => matches!(status, 401 | 403),
        _ => false,
    }
}

/// Time left on a lease that lapses at `expires_at` (RFC 3339).
fn remaining(expires_at: Option<&str>) -> Duration {
    expires_at
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .and_then(|t| {
            (t.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .to_std()
                .ok()
        })
        .unwrap_or_default()
}

/// The `runtime` page's fields a re-registration compares, so a rewrite
/// happens only when something a reader sees changed.
#[must_use]
pub fn registration(harnesses: &[RuntimeHarness], labels: &[String], max_concurrent: u64) -> Value {
    json!({ "harnesses": harnesses, "labels": labels, "max_concurrent": max_concurrent })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_harness_set_leads_with_the_default() {
        assert_eq!(
            harness_set("echo", &["codex".to_owned(), "echo".to_owned()]),
            vec!["echo", "codex"]
        );
    }

    #[test]
    fn the_page_parses_as_a_runtime_instance() {
        let page = runtime_page(
            "laptop:42",
            Some("svc#01abc"),
            &[RuntimeHarness {
                name: "echo".into(),
                version: Some("echo 1.0: \"x\"".into()),
            }],
            &["site:muc".to_owned()],
            4,
        );
        let parsed = escurel_md_free_yaml(&page);
        assert_eq!(parsed["skill"], "runtime");
        assert_eq!(parsed["id"], "laptop-42");
        assert_eq!(parsed["runner_id"], "laptop:42");
        assert_eq!(parsed["labels"], json!(["site:muc"]));
        assert_eq!(parsed["harnesses"][0]["version"], "echo 1.0: \"x\"");
        assert_eq!(parsed["max_concurrent"], 4);
        assert_eq!(parsed["claim_holder"], "svc#01abc");
    }

    /// The frontmatter as JSON. Every value is written as JSON, so a line
    /// split on the first `: ` is enough here.
    fn escurel_md_free_yaml(page: &str) -> Value {
        let fm = page.split("---\n").nth(1).unwrap();
        let mut out = serde_json::Map::new();
        for line in fm.lines() {
            let (k, v) = line.split_once(": ").unwrap();
            let v = serde_json::from_str(v).unwrap_or_else(|_| json!(v));
            out.insert(k.to_owned(), v);
        }
        Value::Object(out)
    }

    /// A gateway that does not answer (here: a closed port, a real
    /// connection refusal) is not a gateway that has no leases: two runners
    /// would both start the event. The claim is retried next poll.
    #[tokio::test]
    async fn a_claim_the_gateway_never_answered_is_retried_not_taken() {
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let client = Client::connect(
            &format!("http://127.0.0.1:{closed}"),
            escurel_client::SecretString::from("t".to_owned()),
        )
        .await
        .unwrap();
        let gate = RuntimeGate::new(
            "b",
            Some("svc"),
            vec!["echo".into()],
            vec![],
            Duration::from_secs(60),
        );
        assert_eq!(gate.claim(&client, "e1").await, Admit::Retry);
    }

    #[test]
    fn only_a_permanent_claim_refusal_fails_open() {
        let rpc = |code: i64, data_code: Option<&str>| Error::JsonRpc {
            code,
            message: String::new(),
            data_code: data_code.map(str::to_owned),
            retryable: None,
        };
        for permanent in [
            rpc(-32601, None),
            rpc(-32001, None),
            rpc(-32602, Some("forbidden")),
            rpc(-32602, Some("event_not_found")),
            Error::Http {
                status: 403,
                body: String::new(),
            },
        ] {
            assert!(permanent_claim_refusal(&permanent), "{permanent}");
        }
        for transient in [
            rpc(-32603, None),
            rpc(-32000, Some("quota_exhausted")),
            Error::Http {
                status: 503,
                body: String::new(),
            },
            Error::Http {
                status: 429,
                body: String::new(),
            },
        ] {
            assert!(!permanent_claim_refusal(&transient), "{transient}");
        }
    }

    #[test]
    fn a_route_needs_every_condition() {
        let gate = RuntimeGate::new(
            "b",
            Some("svc"),
            vec!["echo".into()],
            vec!["has:sap-gui".into()],
            Duration::from_secs(60),
        );
        assert_eq!(gate.holder(), Some("svc#b"));
        assert!(gate.routes(&Requires {
            labels: vec!["has:sap-gui".into()],
            ..Requires::default()
        }));
        assert!(!gate.routes(&Requires {
            harness: Some("codex".into()),
            ..Requires::default()
        }));
        assert!(!gate.routes(&Requires {
            invalid: true,
            ..Requires::default()
        }));
    }
}
