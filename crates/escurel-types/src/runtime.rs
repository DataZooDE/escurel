//! Distributed runtimes (#818): what a runner advertises and what work asks for.
//!
//! A runner registers itself as an instance of the `runtime` system skill
//! (its harnesses, labels and capacity), and a skill page or an event may
//! declare `requires: {harness, labels}`. The runner reads both and skips an
//! inbox item it cannot satisfy *before* claiming it, so routing stays on the
//! runner side and the gateway stays automation-free. The parser lives here
//! because the indexer's validator, the gateway's `capture_event` check and
//! the runner must agree on one grammar.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Skill id of runtime registrations: `markdown/instances/runtime/<slug>.md`.
pub const RUNTIME_SKILL: &str = "runtime";

/// A label is a short token such as `site:muc` or `has:sap-gui`.
pub const MAX_LABEL_LEN: usize = 64;

/// What a skill or an event needs from the runner that takes it.
///
/// Every condition must hold: the runner offers `harness` (its default or an
/// allow-listed one) and carries every label in `labels`. A `requires:` that
/// is declared but malformed is kept as [`Requires::invalid`] and satisfied by
/// nobody: reading it as "no requirement" would route the work to any runner,
/// which is the opposite of what its author asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Declared but unreadable; no runner satisfies it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invalid: bool,
}

impl Requires {
    /// Parse a declared `requires:` value strictly.
    ///
    /// # Errors
    /// A worded reason when `v` is not `{harness?: <name>, labels?: [<label>…]}`.
    pub fn parse(v: &Value) -> Result<Self, String> {
        let Some(map) = v.as_object() else {
            return Err("`requires:` is a map with `harness` and/or `labels`".to_owned());
        };
        let mut out = Self::default();
        for (key, value) in map {
            match key.as_str() {
                "harness" => match value.as_str().map(str::trim) {
                    Some(h) if !h.is_empty() && is_token(h) => out.harness = Some(h.to_owned()),
                    _ => return Err("`requires.harness` is one harness name".to_owned()),
                },
                "labels" => {
                    let Some(items) = value.as_array() else {
                        return Err("`requires.labels` is a list of labels".to_owned());
                    };
                    for item in items {
                        match item.as_str().map(str::trim) {
                            Some(l) if is_label(l) => out.labels.push(l.to_owned()),
                            _ => {
                                return Err(format!(
                                    "`requires.labels` entries are labels such as `site:muc` \
                                     (no spaces or commas, at most {MAX_LABEL_LEN} characters)"
                                ));
                            }
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "`requires:` knows only `harness` and `labels`, not `{other}`"
                    ));
                }
            }
        }
        Ok(out)
    }

    /// The requirement a frontmatter or provenance value declares: `None`
    /// when absent or null, an [`invalid`](Self::invalid) one when malformed.
    #[must_use]
    pub fn declared(v: Option<&Value>) -> Option<Self> {
        let v = v.filter(|v| !v.is_null())?;
        Some(Self::parse(v).unwrap_or(Self {
            invalid: true,
            ..Self::default()
        }))
    }

    /// Both requirements at once: the event's `provenance.requires` on top of
    /// its skill's. It can only narrow: labels add up, and two different
    /// harnesses are satisfied by nobody (a run has one harness).
    #[must_use]
    pub fn and(self, other: Option<Self>) -> Self {
        let Some(other) = other else {
            return self;
        };
        let mut labels = self.labels;
        for l in other.labels {
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
        let conflict = matches!((&self.harness, &other.harness), (Some(a), Some(b)) if a != b);
        Self {
            harness: self.harness.or(other.harness),
            labels,
            invalid: self.invalid || other.invalid || conflict,
        }
    }

    /// Whether a runner offering `harnesses` and carrying `labels` may take the work.
    #[must_use]
    pub fn satisfied_by<H: AsRef<str>, L: AsRef<str>>(
        &self,
        harnesses: &[H],
        labels: &[L],
    ) -> bool {
        !self.invalid
            && self
                .harness
                .as_deref()
                .is_none_or(|h| harnesses.iter().any(|x| x.as_ref() == h))
            && self
                .labels
                .iter()
                .all(|l| labels.iter().any(|x| x.as_ref() == l))
    }
}

/// Parse `ESCUREL_RUNNER_LABELS` (a comma list) into labels.
///
/// # Errors
/// The first entry that is not a label.
pub fn parse_label_list(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !is_label(part) {
            return Err(format!(
                "`{part}` is not a label (no spaces, at most {MAX_LABEL_LEN} characters)"
            ));
        }
        if !out.iter().any(|l| l == part) {
            out.push(part.to_owned());
        }
    }
    Ok(out)
}

/// The instance slug a runner id registers under: `[a-z0-9-]`, so a default
/// id such as `host:1234` still names one page.
#[must_use]
pub fn runtime_slug(runner_id: &str) -> String {
    let mut slug = String::with_capacity(runner_id.len());
    for c in runner_id.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "runner".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The page a runner registers as.
#[must_use]
pub fn runtime_page_id(runner_id: &str) -> String {
    format!(
        "markdown/instances/{RUNTIME_SKILL}/{}.md",
        runtime_slug(runner_id)
    )
}

/// A live claim lease on an event (`claim_event` / `renew_claim`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EventClaim {
    /// Who holds it: the runner's token `sub`, `#<holder_id>` when it sent one.
    pub holder: String,
    /// RFC 3339: when the lease lapses unless renewed.
    pub expires_at: String,
}

/// `claim_event` / `renew_claim` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClaimEventRequest {
    pub event_id: String,
    /// Lease length in seconds; 0 sends nothing (the gateway default).
    pub lease_secs: u64,
    /// Qualifies the holder, so replicas sharing one credential still
    /// exclude each other; the runner sends its ledger's stable identity.
    /// Empty sends nothing.
    pub holder_id: String,
}

/// `claim_event` / `renew_claim` result. `claimed` is `renewed` for a renew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClaimEventResponse {
    pub event_id: String,
    /// The caller holds the lease now.
    #[serde(alias = "renewed")]
    pub claimed: bool,
    /// The current holder (the caller when `claimed`); `None` when nobody
    /// holds it (a processed event).
    pub holder: Option<String>,
    pub expires_at: Option<String>,
    /// The event is already processed: nothing left to claim.
    pub processed: bool,
}

/// One harness a runtime offers, with the CLI version its probe read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuntimeHarness {
    pub name: String,
    /// `<cli> --version`, first line; `None` for an in-process harness or a
    /// probe that failed.
    pub version: Option<String>,
}

/// One registered runtime as `list_runtimes` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuntimeInfo {
    pub runner_id: String,
    pub page_id: String,
    pub harnesses: Vec<RuntimeHarness>,
    pub labels: Vec<String>,
    pub max_concurrent: Option<u64>,
    /// The holder string this runner's claim leases carry (`claim_event`),
    /// so a reader can say which runtime holds an event.
    pub claim_holder: Option<String>,
    /// `online` | `stale` | `stopped` | `unknown` (no status record).
    pub status: String,
    /// RFC 3339: the runner's last status report.
    pub last_seen: Option<String>,
    /// Seconds since `last_seen`.
    pub age_s: Option<u64>,
    /// The age past which the runtime reads `stale` (3 x its reporting interval).
    pub stale_after_s: Option<u64>,
    /// The runner's own version and run counts, from its status record.
    pub version: Option<String>,
    pub live_runs: u64,
}

/// `list_runtimes` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListRuntimesResponse {
    pub runtimes: Vec<RuntimeInfo>,
}

fn is_token(s: &str) -> bool {
    s.len() <= MAX_LABEL_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_LABEL_LEN
        && s.chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != ',' && c != '"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_events_requirement_only_narrows_its_skills() {
        let skill = Requires {
            labels: vec!["has:sap-gui".into()],
            ..Requires::default()
        };
        let both = skill.clone().and(Some(Requires {
            labels: vec!["site:hq".into()],
            ..Requires::default()
        }));
        assert_eq!(both.labels, ["has:sap-gui", "site:hq"]);
        assert!(!both.satisfied_by(&["echo"], &["has:sap-gui"]));
        let loosened = skill.clone().and(Some(Requires::default()));
        assert!(
            !loosened.satisfied_by(&["echo"], &[] as &[&str]),
            "an empty one loosens nothing"
        );
        let two = Requires {
            harness: Some("claude".into()),
            ..Requires::default()
        }
        .and(Some(Requires {
            harness: Some("codex".into()),
            ..Requires::default()
        }));
        assert!(!two.satisfied_by(&["claude", "codex"], &[] as &[&str]));
        assert_eq!(skill.clone().and(None), skill);
    }
    use serde_json::json;

    #[test]
    fn a_requirement_is_met_only_by_the_harness_and_every_label() {
        let r = Requires::parse(&json!({"harness": "codex", "labels": ["site:muc"]})).unwrap();
        assert!(r.satisfied_by(&["echo", "codex"], &["site:muc", "has:x"]));
        assert!(!r.satisfied_by(&["echo"], &["site:muc"]));
        assert!(!r.satisfied_by(&["codex"], &["site:hq"]));
        assert!(Requires::default().satisfied_by::<&str, &str>(&[], &[]));
    }

    #[test]
    fn a_malformed_requirement_is_kept_and_met_by_nobody() {
        for bad in [
            json!("codex"),
            json!({"harness": ""}),
            json!({"labels": "site:muc"}),
            json!({"labels": ["two words"]}),
            json!({"label": ["x"]}),
        ] {
            assert!(Requires::parse(&bad).is_err(), "{bad}");
            let r = Requires::declared(Some(&bad)).unwrap();
            assert!(r.invalid, "{bad}");
            assert!(!r.satisfied_by(&["echo"], &["x"]), "{bad}");
        }
        assert_eq!(Requires::declared(None), None);
        assert_eq!(Requires::declared(Some(&Value::Null)), None);
    }

    #[test]
    fn labels_and_slugs() {
        assert_eq!(
            parse_label_list(" site:muc, has:sap-gui,,site:muc ").unwrap(),
            vec!["site:muc", "has:sap-gui"]
        );
        assert!(parse_label_list("a b").is_err());
        assert_eq!(runtime_slug("laptop.local:4242"), "laptop-local-4242");
        assert_eq!(runtime_slug("::"), "runner");
        assert_eq!(
            runtime_page_id("B"),
            "markdown/instances/runtime/b.md".to_owned()
        );
    }
}
