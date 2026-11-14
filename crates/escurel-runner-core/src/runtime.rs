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
//!   lease, renewed by [`RuntimeGate::renew_held`] on its own ticker (a third
//!   of the lease), so exactly one runner starts it and a slow inbox tick
//!   never lets a running event's lease lapse. The ledger stays the local
//!   idempotency authority.
//!
//! A gateway older than the claim tools answers `-32601`, and an event the
//! gateway does not store answers `event_not_found`; the gate then admits
//! what it can route, which is the single-runner behaviour. A claim the
//! gateway REFUSED for this identity (not an admin) takes nothing and logs
//! the fix, on every runner, lone or not: a runner credential must be an
//! admin, and such a runner is misconfigured, not talking to an old gateway. A claim the gateway did not answer (timeout, transport, `5xx`,
//! an unexpected invalid-params) is retried on the next poll, never taken.
//! Admission is one gateway call per inbox event, with up to
//! `ESCUREL_RUNNER_ADMIT_CONCURRENCY` in flight per poll (#828); the answers
//! are taken in inbox order.

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

/// One harness a runner offers, as its probe needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpec {
    pub name: String,
    /// The CLI whose `--version` is probed; `None` for an in-process harness.
    pub cli: Option<String>,
    /// What the harness declares (`Harness::supports_inject`), carried onto
    /// the runtime page so a client reads it instead of guessing.
    pub supports_inject: bool,
}

/// Probe each harness: the first line of `<cli> --version`. An in-process
/// harness (no CLI) is available with no version; a CLI whose probe fails is
/// UNAVAILABLE, with the reason: a host without the binary (or with one that
/// cannot start) cannot run the work, so it must not be offered.
pub async fn probe_harnesses(harnesses: &[HarnessSpec]) -> Vec<RuntimeHarness> {
    let mut out = Vec::with_capacity(harnesses.len());
    for spec in harnesses {
        let probed = match &spec.cli {
            Some(bin) => probe_version(bin).await,
            None => Ok(None),
        };
        out.push(match probed {
            Ok(version) => RuntimeHarness {
                name: spec.name.clone(),
                version,
                supports_inject: Some(spec.supports_inject),
                ..RuntimeHarness::default()
            },
            Err(why) => RuntimeHarness {
                name: spec.name.clone(),
                version: None,
                available: false,
                probe_error: Some(why),
                supports_inject: Some(spec.supports_inject),
            },
        });
    }
    out
}

async fn probe_version(bin: &str) -> Result<Option<String>, String> {
    let run = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PROBE_TIMEOUT, run)
        .await
        .map_err(|_| format!("`{bin} --version` timed out"))?
        .map_err(|e| format!("`{bin} --version` could not start: {e}"))?;
    if !output.status.success() {
        return Err(format!("`{bin} --version` exited with {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|line| line.chars().take(MAX_VERSION_LEN).collect()))
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
    /// The gateway refused this runner's identity a claim (it is not an
    /// admin): nothing is taken, since another runner may hold the event.
    Refused,
}

impl Admit {
    /// The decision's name, as `/debug/admission` reports it.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Take => "take",
            Self::Unroutable => "unroutable",
            Self::Elsewhere(_) => "elsewhere",
            Self::Done => "done",
            Self::Retry => "retry",
            Self::Refused => "refused",
        }
    }
}

/// How a claim the gateway answered with an error is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRefusal {
    /// The gateway has no claim tools (`-32601`): admit as a lone runner.
    Unsupported,
    /// The gateway does not store the event (`event_not_found`, e.g. a
    /// synthetic webhook event): nothing to coordinate on, admit it.
    NotStored,
    /// This identity may not claim (`forbidden`, `-32001`, `-32003`, `401`,
    /// `403`): take nothing.
    Forbidden,
    /// Anything else may be the gateway being slow or a bug: retry.
    Transient,
}

struct Held {
    mine: bool,
    until: Instant,
}

/// One event's recorded decisions. The webhook path's outcome is kept apart
/// from the latest gate decision: the poller re-admits the same inbox event
/// every tick, and would otherwise overwrite what the webhook did with it.
#[derive(Default)]
struct Decisions {
    last: Option<String>,
    webhook: Option<String>,
}

/// How many admission decisions `/debug/admission` remembers.
const MAX_DECISIONS: usize = 10_000;

/// The runner's routing + claim gate, shared by the poller and the webhook.
pub struct RuntimeGate {
    /// The ledger's stable identity, sent as `holder_id`.
    holder_id: String,
    /// `<sub>#<holder_id>`, as the gateway writes the holder.
    holder: Option<String>,
    harnesses: Vec<String>,
    labels: Vec<String>,
    lease: Duration,
    /// Take only work whose skill asks for this runner (`ESCUREL_RUNNER_EXCLUSIVE`).
    exclusive: bool,
    skills: Mutex<Option<(Instant, HashMap<String, Requires>)>>,
    held: Mutex<HashMap<String, Held>>,
    claims_unsupported: AtomicBool,
    told: Mutex<HashSet<String>>,
    /// Harnesses whose CLI probe failed: never offered for `requires.harness`.
    unavailable: Mutex<HashSet<String>>,
    /// What this runner decided per event (`/debug/admission`): a test's
    /// positive "this runner had its chance" signal before it asserts an
    /// absence.
    decisions: Mutex<HashMap<String, Decisions>>,
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
            exclusive: false,
            skills: Mutex::new(None),
            held: Mutex::new(HashMap::new()),
            claims_unsupported: AtomicBool::new(false),
            told: Mutex::new(HashSet::new()),
            unavailable: Mutex::new(HashSet::new()),
            decisions: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this runner is effectively alone: no labels and not exclusive,
    /// so no other runtime exists to route work to instead. Only such a
    /// runner may keep an undecidable webhook trigger on its ledger (#440).
    #[must_use]
    pub fn is_lone(&self) -> bool {
        self.labels.is_empty() && !self.exclusive
    }

    /// The claim lease this gate asks for.
    #[must_use]
    pub fn lease(&self) -> Duration {
        self.lease
    }

    /// Apply a harness probe: a harness whose probe failed is no longer
    /// offered for `requires.harness`, one that recovered is again.
    pub fn apply_probe(&self, probed: &[RuntimeHarness]) {
        let down: HashSet<String> = probed
            .iter()
            .filter(|h| !h.available)
            .map(|h| h.name.clone())
            .collect();
        let mut unavailable = self.unavailable.lock().unwrap_or_else(|e| e.into_inner());
        if *unavailable != down {
            for h in probed.iter().filter(|h| !h.available) {
                tracing::warn!(
                    target: "escurel_runner",
                    harness = %h.name,
                    error = h.probe_error.as_deref().unwrap_or_default(),
                    "harness CLI probe failed; work that requires this harness is left for \
                     another runtime"
                );
            }
            *unavailable = down;
        }
    }

    /// The harnesses this runner can run now, default first.
    fn offered(&self) -> Vec<String> {
        let unavailable = self.unavailable.lock().unwrap_or_else(|e| e.into_inner());
        self.harnesses
            .iter()
            .filter(|h| !unavailable.contains(*h))
            .cloned()
            .collect()
    }

    /// The last admission decision for `event_id` (from either path), and
    /// what the webhook path did with it, if it saw the event.
    #[must_use]
    pub fn decision(&self, event_id: &str) -> Option<(Option<String>, Option<String>)> {
        self.decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(event_id)
            .map(|d| (d.last.clone(), d.webhook.clone()))
    }

    fn note(&self, event_id: &str, apply: impl FnOnce(&mut Decisions)) {
        let mut decisions = self.decisions.lock().unwrap_or_else(|e| e.into_inner());
        if decisions.len() >= MAX_DECISIONS && !decisions.contains_key(event_id) {
            decisions.clear();
        }
        apply(decisions.entry(event_id.to_owned()).or_default());
    }

    /// Record the gate's own decision about `event_id`.
    pub fn note_decision(&self, event_id: &str, decision: &str) {
        self.note(event_id, |d| d.last = Some(decision.to_owned()));
    }

    /// Record what the webhook path did with `event_id`. It is also the
    /// latest decision, but unlike that it is never overwritten by the poller.
    pub fn note_webhook(&self, event_id: &str, outcome: &str) {
        self.note(event_id, |d| {
            d.last = Some(outcome.to_owned());
            d.webhook = Some(outcome.to_owned());
        });
    }

    /// Take only work whose SKILL page's `requires:` names one of this
    /// runner's labels or a harness it offers other than its default — a
    /// laptop or an on-prem host that should not race the central runner for
    /// the whole inbox. An event's `provenance.requires` never makes work
    /// eligible here: a capturer could otherwise steer any skill onto it.
    #[must_use]
    pub fn exclusive(mut self, on: bool) -> Self {
        self.exclusive = on;
        self
    }

    /// Whether a skill's requirement asks for this runner in particular.
    fn asks_for_me(&self, skill: &Requires) -> bool {
        let offered = self.offered();
        skill.labels.iter().any(|l| self.labels.contains(l))
            || skill
                .harness
                .as_ref()
                .is_some_and(|h| self.harnesses.first() != Some(h) && offered.contains(h))
    }

    /// The holder string the gateway writes for this runner's leases.
    #[must_use]
    pub fn holder(&self) -> Option<&str> {
        self.holder.as_deref()
    }

    /// Whether this runner satisfies everything `event` and its skill require.
    #[must_use]
    pub fn routes(&self, event: &Requires) -> bool {
        event.satisfied_by(&self.offered(), &self.labels)
    }

    /// Route, then claim. Never claims an event it cannot route.
    pub async fn admit(&self, client: &Client, event: &Event) -> Admit {
        let admit = self.decide(client, event).await;
        self.note_decision(&event.event_id, admit.as_str());
        admit
    }

    async fn decide(&self, client: &Client, event: &Event) -> Admit {
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
        if self.exclusive && !skill.as_ref().is_some_and(|r| self.asks_for_me(r)) {
            self.tell_once(&event.event_id, || {
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %event.event_id,
                    label_skill = %event.label_skill,
                    "exclusive runner: the skill does not ask for this runner; left unclaimed"
                );
            });
            return Admit::Unroutable;
        }
        // The event can only narrow its skill's requirement, never loosen it.
        let effective = skill.unwrap_or_default().and(declared);
        if !self.routes(&effective) {
            let r = &effective;
            self.tell_once(&event.event_id, || {
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %event.event_id,
                    label_skill = %event.label_skill,
                    requires = %json!(r),
                    harnesses = ?self.offered(),
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
                // A lease of ours that already lapsed is not renewed: the
                // gateway may have let another runner take it meanwhile, and a
                // renew during its boot grace would re-create it regardless.
                // It is claimed afresh, which a live holder wins.
                Some(h) => h.mine && h.until > now,
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
            Ok(Err(e)) => match classify_claim_error(&e) {
                ClaimRefusal::Unsupported => self.no_answer(event_id, true, &e),
                ClaimRefusal::NotStored => self.no_answer(event_id, false, &e),
                ClaimRefusal::Forbidden => self.forbidden(event_id, &e),
                ClaimRefusal::Transient => {
                    if matches!(e, Error::JsonRpc { code: -32602, .. }) {
                        // Not a known benign refusal: a bug or a config the
                        // gateway rejects (an out-of-range lease). Never a
                        // reason to run unclaimed, and worth an error.
                        tracing::error!(
                            target: "escurel_runner",
                            event_id,
                            error = %e,
                            "claim_event refused its arguments; the event waits (nothing is taken)"
                        );
                    }
                    self.unanswered(event_id, &e)
                }
            },
            Err(_) => self.unanswered(event_id, &"claim_event timed out"),
        }
    }

    /// Renew every lease this runner holds that has not lapsed — the runs it
    /// is executing — independent of the inbox loop, so a slow tick never
    /// lets one lapse. A lease of ours that already lapsed is dropped from
    /// the book (it is claimed afresh if the event shows up again), never
    /// renewed. Call it every third of the lease.
    pub async fn renew_held(&self, client: &Client) {
        if self.claims_unsupported.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        let due: Vec<String> = {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|_, h| !h.mine || h.until > now);
            held.iter()
                .filter(|(_, h)| h.mine)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for event_id in due {
            let req = ClaimEventRequest {
                event_id: event_id.clone(),
                lease_secs: self.lease.as_secs().max(1),
                holder_id: self.holder_id.clone(),
            };
            match tokio::time::timeout(ADMIT_CALL_TIMEOUT, client.renew_claim(req)).await {
                Ok(Ok(r)) => {
                    if !r.processed && !r.claimed {
                        tracing::error!(
                            target: "escurel_runner",
                            event_id = %event_id,
                            holder = r.holder.as_deref().unwrap_or_default(),
                            "a lease this runner held is now another holder's"
                        );
                    }
                    self.record(&event_id, &r);
                }
                Ok(Err(e)) => tracing::warn!(
                    target: "escurel_runner",
                    event_id = %event_id,
                    error = %e,
                    "renew_claim failed; retried on the next renewal tick"
                ),
                Err(_) => tracing::warn!(
                    target: "escurel_runner",
                    event_id = %event_id,
                    "renew_claim timed out; retried on the next renewal tick"
                ),
            }
        }
    }

    /// The gateway will not let this identity claim: the runner credential is
    /// neither `escurel:runner` nor an admin. Running the event anyway could race a runner that holds
    /// it, and a lone runner cannot know it is alone (owner decision
    /// 2026-11-13: fail closed for every runner), so it takes nothing and
    /// says how to fix it.
    fn forbidden(&self, event_id: &str, e: &dyn std::fmt::Display) -> Admit {
        self.tell_once("\0claim-forbidden", || {
            tracing::error!(
                target: "escurel_runner",
                event_id,
                error = %e,
                "claim_event refused: the runner credential needs escurel:runner or escurel:admin \
                 to claim events; this runner takes no inbox work until it has one (logged once)"
            );
        });
        Admit::Refused
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

    /// An event the gateway does not store (a synthetic webhook) or an image
    /// without the tool (`unsupported`) falls back to the single-runner
    /// behaviour — the ledger alone decides.
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

/// What a claim error means. Only a gateway without the tool (`-32601`) and
/// an event it does not store (`-32602` `event_not_found`) are reasons to
/// admit as a lone runner. An identity refusal (`forbidden`, `-32001` admin
/// required, `-32003`, `401`/`403`) takes nothing. Everything else — another
/// invalid-params answer (an out-of-range lease is one), a timeout, a
/// transport failure, a `5xx`/`429` — is retried: none of it says that no
/// other runner holds the event.
fn classify_claim_error(e: &Error) -> ClaimRefusal {
    match e {
        Error::JsonRpc { code: -32601, .. } => ClaimRefusal::Unsupported,
        Error::JsonRpc {
            code: -32602,
            data_code,
            ..
        } => match data_code.as_deref() {
            Some("event_not_found") => ClaimRefusal::NotStored,
            Some("forbidden") => ClaimRefusal::Forbidden,
            _ => ClaimRefusal::Transient,
        },
        Error::JsonRpc {
            code: -32001 | -32003,
            ..
        } => ClaimRefusal::Forbidden,
        Error::Http {
            status: 401 | 403, ..
        } => ClaimRefusal::Forbidden,
        _ => ClaimRefusal::Transient,
    }
}

/// Whether `e` says this IDENTITY may not do what it asked (`forbidden`, `-32001`, `-32003`,
/// `401`/`403`) — the same reading a refused claim gets — rather than a fault worth retrying.
#[must_use]
pub fn is_identity_refusal(e: &Error) -> bool {
    matches!(classify_claim_error(e), ClaimRefusal::Forbidden)
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
                ..RuntimeHarness::default()
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
    fn only_a_missing_tool_or_an_unstored_event_fails_open() {
        let rpc = |code: i64, data_code: Option<&str>| Error::JsonRpc {
            code,
            message: String::new(),
            data_code: data_code.map(str::to_owned),
            retryable: None,
        };
        let http = |status: u16| Error::Http {
            status,
            body: String::new(),
        };
        let cases = [
            (rpc(-32601, None), ClaimRefusal::Unsupported),
            (
                rpc(-32602, Some("event_not_found")),
                ClaimRefusal::NotStored,
            ),
            (rpc(-32602, Some("forbidden")), ClaimRefusal::Forbidden),
            (rpc(-32001, None), ClaimRefusal::Forbidden),
            (rpc(-32003, None), ClaimRefusal::Forbidden),
            (http(401), ClaimRefusal::Forbidden),
            (http(403), ClaimRefusal::Forbidden),
            // The gateway refusing an out-of-range lease is no licence to run
            // unclaimed (crew C-F1).
            (rpc(-32602, None), ClaimRefusal::Transient),
            (rpc(-32602, Some("bad_lease")), ClaimRefusal::Transient),
            (rpc(-32603, None), ClaimRefusal::Transient),
            (
                rpc(-32000, Some("quota_exhausted")),
                ClaimRefusal::Transient,
            ),
            (http(503), ClaimRefusal::Transient),
            (http(429), ClaimRefusal::Transient),
        ];
        for (e, want) in cases {
            assert_eq!(classify_claim_error(&e), want, "{e}");
        }
    }

    #[tokio::test]
    async fn a_failed_probe_makes_a_harness_unavailable_and_unrouted() {
        let spec = |name: &str, cli: Option<&str>| HarnessSpec {
            name: name.to_owned(),
            cli: cli.map(str::to_owned),
            supports_inject: false,
        };
        let probed = probe_harnesses(&[
            spec("echo", None),
            spec("codex", Some("/nonexistent/codex")),
        ])
        .await;
        assert!(probed[0].available);
        assert!(!probed[1].available);
        assert!(
            probed[1]
                .probe_error
                .as_deref()
                .is_some_and(|e| e.contains("/nonexistent/codex")),
            "{probed:?}"
        );
        let gate = RuntimeGate::new(
            "b",
            Some("svc"),
            vec!["echo".into(), "codex".into()],
            vec![],
            Duration::from_secs(60),
        );
        let codex = Requires {
            harness: Some("codex".into()),
            ..Requires::default()
        };
        assert!(gate.routes(&codex));
        gate.apply_probe(&probed);
        assert!(!gate.routes(&codex), "a CLI that is not there runs nothing");
        assert!(gate.routes(&Requires::default()));
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
