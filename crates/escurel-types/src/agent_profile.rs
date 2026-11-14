//! Agent profiles (#823): an `agent` instance page names the harness, model,
//! skills and least-privilege scopes a run executes under.
//!
//! One parser for the three readers — the runner choosing an agent for a run,
//! the gateway enforcing its scopes, and `validate` grading the page — so the
//! three cannot disagree about what a page grants. The parse is deliberately
//! **fail-closed** on the scopes: a `scopes.tools` or `scopes.write` that is
//! present but malformed grants nothing, never everything.

use serde_json::Value;

/// Skill id of agent profile pages.
pub const AGENT_SKILL: &str = "agent";

/// The page an agent profile lives at.
#[must_use]
pub fn agent_page_id(id: &str) -> String {
    format!("markdown/instances/{AGENT_SKILL}/{id}.md")
}

/// Whether `id` can name an agent: what a page id, a token subject and a
/// wikilink can all carry unambiguously.
#[must_use]
pub fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Whether a page can serve as an agent profile: an INSTANCE of the `agent`
/// skill that is not archived. The one rule the runner (choosing an agent) and
/// the gateway (enforcing its scopes) both apply, so they cannot disagree about
/// which pages are agents.
#[must_use]
pub fn is_usable_agent_page(skill: &str, is_instance: bool, frontmatter: &Value) -> bool {
    skill == AGENT_SKILL
        && is_instance
        && frontmatter.get("archived").and_then(Value::as_bool) != Some(true)
}

/// One agent profile, read from its page's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentProfile {
    pub id: String,
    pub harness: Option<String>,
    pub model: Option<String>,
    /// The skills this agent may run.
    pub skills: Vec<String>,
    /// `scopes.tools`: `None` = no tool restriction.
    pub tools: Option<Vec<String>>,
    /// `scopes.write`: page-id globs; `None` = no extra write restriction.
    pub write: Option<Vec<String>>,
    pub max_concurrent: Option<u32>,
    /// `max_concurrent:` is present but not a positive whole number. Fails
    /// CLOSED: the runner refuses this agent's runs (visibly, naming the key)
    /// rather than running them uncapped.
    pub max_concurrent_invalid: bool,
    /// `token_epoch:` (Unix seconds, #829): every token for this agent issued
    /// before it is refused (`agent_token_revoked`). A value that is not a
    /// whole number revokes everything: fail closed.
    pub token_epoch: Option<u64>,
}

impl AgentProfile {
    /// Read a profile from an `agent` page's frontmatter (JSON, as `expand`
    /// returns it).
    #[must_use]
    pub fn from_frontmatter(id: &str, fm: &Value) -> Self {
        let text = |key: &str| {
            fm.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let scopes = fm.get("scopes");
        Self {
            id: id.to_owned(),
            harness: text("harness"),
            model: text("model"),
            skills: link_ids(fm.get("skills"), "skill"),
            tools: scopes.and_then(|s| s.get("tools")).map(strings_or_nothing),
            write: scopes.and_then(|s| s.get("write")).map(strings_or_nothing),
            max_concurrent: fm
                .get("max_concurrent")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
            max_concurrent_invalid: fm
                .get("max_concurrent")
                .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0)),
            token_epoch: fm
                .get("token_epoch")
                .map(|v| v.as_u64().unwrap_or(u64::MAX)),
        }
    }

    /// Whether a token for this agent issued at `issued_at` (its `iat`) still
    /// stands: issued at or after [`Self::token_epoch`]. A token with no `iat`
    /// cannot be placed against the epoch, so it never stands.
    #[must_use]
    pub fn token_stands(&self, issued_at: Option<u64>) -> bool {
        match (issued_at, self.token_epoch) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(iat), Some(epoch)) => iat >= epoch,
        }
    }

    /// Whether this agent may run `skill`.
    #[must_use]
    pub fn lists_skill(&self, skill: &str) -> bool {
        self.skills.iter().any(|s| s == skill)
    }

    /// Whether this agent may call `tool`.
    #[must_use]
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| t == tool))
    }

    /// Whether this agent may write `page_id`.
    #[must_use]
    pub fn allows_write(&self, page_id: &str) -> bool {
        self.write
            .as_ref()
            .is_none_or(|globs| globs.iter().any(|g| glob_match(g, page_id)))
    }
}

/// A string list, or an empty one for anything else: a scope that cannot be
/// read grants nothing.
fn strings_or_nothing(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The ids a wikilink field names in `namespace` — one link or a list. YAML
/// reads an unquoted `[[skill::x]]` as the nested list `[["skill::x"]]`, so
/// both spellings are accepted. For the `skill` namespace a bare `[[x]]` (the
/// skill page itself) counts too.
#[must_use]
pub fn link_ids(v: Option<&Value>, namespace: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_links(v, namespace, &mut out);
    out
}

/// The single id a one-link field names in `namespace`, e.g. a skill's
/// `agent: "[[agent::x]]"`.
#[must_use]
pub fn link_id(v: Option<&Value>, namespace: &str) -> Option<String> {
    let mut ids = link_ids(v, namespace);
    (ids.len() == 1).then(|| ids.remove(0))
}

fn collect_links(v: Option<&Value>, namespace: &str, out: &mut Vec<String>) {
    match v {
        Some(Value::String(s)) => {
            let inner = s
                .trim()
                .strip_prefix("[[")
                .and_then(|r| r.strip_suffix("]]"))
                .unwrap_or(s.trim());
            if let Some(id) = link_target(inner, namespace) {
                out.push(id);
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                collect_links(Some(item), namespace, out);
            }
        }
        _ => {}
    }
}

/// `ns::id` (or a bare `id` in the `skill` namespace) → `id`.
fn link_target(inner: &str, namespace: &str) -> Option<String> {
    let id = match inner.split_once("::") {
        Some((ns, id)) if ns == namespace => id,
        Some(_) => return None,
        None if namespace == "skill" => inner,
        None => return None,
    };
    let id = id.split(['|', '#']).next().unwrap_or("").trim();
    is_valid_agent_id(id).then(|| id.to_owned())
}

/// The most `**` one glob may carry. Each one multiplies the backtracking, and
/// every write an agent makes runs its globs; four is plenty for a page path.
pub const MAX_DOUBLE_STARS: usize = 4;

/// Whether `pattern` stays within [`MAX_DOUBLE_STARS`].
#[must_use]
pub fn glob_is_bounded(pattern: &str) -> bool {
    pattern.matches("**").count() <= MAX_DOUBLE_STARS
}

/// Match a page id against a path glob: `*` matches within one `/`-separated
/// segment, `**` matches across segments (including none), `?` one non-`/`
/// character. Everything else is literal. A glob over [`MAX_DOUBLE_STARS`]
/// matches nothing (fail closed: a scope grants, so an unusable one grants
/// nothing).
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    if !glob_is_bounded(pattern) {
        return false;
    }
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                // `**/` may also match zero segments.
                let rest_after_slash = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=t.len()).any(|i| go(rest, &t[i..]) || go(rest_after_slash, &t[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                let mut i = 0;
                loop {
                    if go(rest, &t[i..]) {
                        return true;
                    }
                    if i == t.len() || t[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(b'?') => t.first().is_some_and(|c| *c != b'/') && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_glob_with_too_many_double_stars_matches_nothing() {
        let five = "**/**/**/**/**";
        assert!(!glob_is_bounded(five));
        assert!(!glob_match(five, "a/b"));
        assert!(glob_match("**/**/**/**", "a/b"));
    }

    #[test]
    fn a_single_star_stays_inside_one_segment() {
        assert!(glob_match(
            "markdown/instances/note/*",
            "markdown/instances/note/x.md"
        ));
        assert!(!glob_match(
            "markdown/instances/note/*",
            "markdown/instances/customer/x.md"
        ));
        assert!(!glob_match(
            "markdown/instances/*",
            "markdown/instances/note/x.md"
        ));
        assert!(!glob_match(
            "markdown/instances/note*",
            "markdown/instances/note/x.md"
        ));
    }

    #[test]
    fn a_double_star_crosses_segments() {
        assert!(glob_match("markdown/**", "markdown/instances/note/x.md"));
        assert!(glob_match("**/x.md", "markdown/instances/note/x.md"));
        assert!(glob_match("markdown/**/x.md", "markdown/x.md"));
        assert!(!glob_match("markdown/**/y.md", "markdown/instances/x.md"));
    }

    #[test]
    fn literals_and_question_marks_are_exact() {
        assert!(glob_match("a/b?.md", "a/b1.md"));
        assert!(!glob_match("a/b?.md", "a/b/.md"));
        assert!(!glob_match("a/b.md", "a/b.mdx"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
    }

    #[test]
    fn links_parse_quoted_unquoted_and_bare() {
        let fm = json!({
            "skills": ["[[skill::note]]", [["skill::customer"]], "[[triage]]", "[[agent::x]]"],
        });
        assert_eq!(
            link_ids(fm.get("skills"), "skill"),
            vec!["note", "customer", "triage"]
        );
        assert_eq!(
            link_id(Some(&json!("[[agent::researcher]]")), "agent"),
            Some("researcher".to_owned())
        );
        assert_eq!(
            link_id(Some(&json!([["agent::researcher"]])), "agent"),
            Some("researcher".to_owned())
        );
        assert_eq!(link_id(Some(&json!("[[researcher]]")), "agent"), None);
        assert_eq!(link_id(Some(&json!("[[agent::../x]]")), "agent"), None);
    }

    #[test]
    fn only_an_unarchived_agent_instance_is_a_usable_agent_page() {
        let fm = json!({});
        assert!(is_usable_agent_page(AGENT_SKILL, true, &fm));
        assert!(
            !is_usable_agent_page(AGENT_SKILL, false, &fm),
            "the skill page itself"
        );
        assert!(!is_usable_agent_page("note", true, &fm));
        assert!(!is_usable_agent_page(
            AGENT_SKILL,
            true,
            &json!({ "archived": true })
        ));
    }

    #[test]
    fn a_profile_reads_its_fields() {
        let fm = json!({
            "harness": "echo", "model": "m1", "skills": ["[[skill::note]]"],
            "scopes": {"tools": ["expand", "update_page"], "write": ["markdown/instances/note/*"]},
            "max_concurrent": 2,
        });
        let p = AgentProfile::from_frontmatter("r", &fm);
        assert_eq!(p.harness.as_deref(), Some("echo"));
        assert_eq!(p.max_concurrent, Some(2));
        assert!(p.lists_skill("note") && !p.lists_skill("customer"));
        assert!(p.allows_tool("expand") && !p.allows_tool("search"));
        assert!(p.allows_write("markdown/instances/note/a.md"));
        assert!(!p.allows_write("markdown/instances/customer/a.md"));
    }

    #[test]
    fn absent_scopes_restrict_nothing_and_malformed_ones_grant_nothing() {
        let open = AgentProfile::from_frontmatter("r", &json!({}));
        assert!(open.allows_tool("search") && open.allows_write("anything"));

        let broken = AgentProfile::from_frontmatter(
            "r",
            &json!({"scopes": {"tools": "search", "write": {"a": 1}}, "max_concurrent": 0}),
        );
        assert!(!broken.allows_tool("search"));
        assert!(!broken.allows_write("markdown/instances/note/a.md"));
        assert_eq!(broken.max_concurrent, None);
    }
}
