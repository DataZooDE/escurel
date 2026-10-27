//! The service-role gate (`ESCUREL_AUTH_ROLE_TOOLS`): a token whose groups carry a configured role
//! value may call only that role's tools.
//!
//! Data ACLs only ever widen access, so they cannot say "this caller may call fewer tools". A
//! worker running under a service identity must be unable to promote or discard drafts, decide
//! changesets, write pages or administer, even when it calls `/mcp` directly. The allow-list is an
//! operator decision about a credential, so it lives in the auth configuration rather than in skill
//! pages a tenant admin edits. Which skills the role may read or propose is data and stays in the
//! skill's `acl:` block (`read_events`, `read_referenced`, `propose`). Design:
//! `docs/design/service-role-gate.md`.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

/// Role value → the tools a token carrying it may call. Empty = no role is restricted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleTools {
    roles: BTreeMap<String, BTreeSet<String>>,
}

impl RoleTools {
    /// Parse `role=tool,tool;role=tool`. Whitespace around items is ignored; an empty string is
    /// no restriction.
    ///
    /// # Errors
    /// A malformed entry, a duplicate role, a role in the reserved `escurel:` namespace (minters
    /// strip it, so a minted child would lose the restriction), an unknown tool, or a tool that is
    /// admin- or operator-scoped (a restricted role never reaches one).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut roles = BTreeMap::new();
        for entry in raw.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let Some((role, tools)) = entry.split_once('=') else {
                return Err(format!("`{entry}` is not `role=tool,tool`"));
            };
            let role = role.trim();
            if role.is_empty() {
                return Err(format!("`{entry}` names no role"));
            }
            if role.starts_with("escurel:") {
                return Err(format!(
                    "role `{role}` is in the reserved `escurel:` namespace, which minted tokens drop"
                ));
            }
            let mut allowed = BTreeSet::new();
            for tool in tools.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                let tool = crate::mcp::canonical_tool(tool);
                match crate::mcp::tool_scope(&tool) {
                    None => return Err(format!("role `{role}`: unknown tool `{tool}`")),
                    Some(scope) if scope != "agent" => {
                        return Err(format!(
                            "role `{role}`: `{tool}` is an {scope} tool; a restricted role never reaches one"
                        ));
                    }
                    Some(_) => {}
                }
                allowed.insert(tool);
            }
            if roles.insert(role.to_owned(), allowed).is_some() {
                return Err(format!("role `{role}` is listed twice"));
            }
        }
        Ok(Self { roles })
    }

    /// The configured role values.
    pub fn roles(&self) -> impl Iterator<Item = &str> {
        self.roles.keys().map(String::as_str)
    }

    /// The tools a caller with `groups` may call, or `None` when it carries no configured role
    /// (unrestricted). Two configured roles yield the intersection: each is a ceiling.
    #[must_use]
    pub fn restriction(&self, groups: &[String]) -> Option<BTreeSet<String>> {
        let mut out: Option<BTreeSet<String>> = None;
        for g in groups {
            if let Some(tools) = self.roles.get(g) {
                out = Some(match out {
                    None => tools.clone(),
                    Some(prev) => prev.intersection(tools).cloned().collect(),
                });
            }
        }
        out
    }
}

/// `403` for a restricted token at a non-tool surface (`/ingest`, `/ingest/upload`, `/blob`,
/// `/ws`): the gate is deny-by-default and those surfaces are not in any allow-list.
pub(crate) fn surface_refusal(
    state: &crate::server::AppState,
    ctx: Option<&escurel_auth::AuthContext>,
    surface: &str,
) -> Option<axum::response::Response> {
    let ctx = ctx?;
    state.role_tools.restriction(&ctx.groups)?;
    Some(
        (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "role_restricted",
                "message": format!("this credential's role does not permit `{surface}`"),
            })),
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::RoleTools;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_roles_and_tools_and_ignores_whitespace() {
        let r = RoleTools::parse(" app:worker = list_events, expand ; app:reader=search ").unwrap();
        assert_eq!(r.roles().collect::<Vec<_>>(), ["app:reader", "app:worker"]);
        let tools = r.restriction(&v(&["app:worker"])).unwrap();
        assert_eq!(
            tools.into_iter().collect::<Vec<_>>(),
            ["expand", "list_events"]
        );
    }

    #[test]
    fn an_empty_value_restricts_nobody() {
        let r = RoleTools::parse("").unwrap();
        assert!(r.restriction(&v(&["app:worker"])).is_none());
    }

    #[test]
    fn a_token_without_a_configured_role_is_unrestricted() {
        let r = RoleTools::parse("app:worker=expand").unwrap();
        assert!(r.restriction(&v(&["editors"])).is_none());
        assert!(r.restriction(&[]).is_none());
    }

    #[test]
    fn two_roles_yield_the_intersection() {
        let r = RoleTools::parse("a:x=expand,search;a:y=expand,list_events").unwrap();
        let tools = r.restriction(&v(&["a:x", "a:y"])).unwrap();
        assert_eq!(tools.into_iter().collect::<Vec<_>>(), ["expand"]);
    }

    #[test]
    fn an_alias_is_stored_under_its_canonical_name() {
        let r = RoleTools::parse("app:worker=run_stored_query").unwrap();
        assert!(
            r.restriction(&v(&["app:worker"]))
                .unwrap()
                .contains("query_instance")
        );
    }

    #[test]
    fn refuses_unknown_tools_admin_tools_reserved_roles_and_duplicates() {
        for (raw, needle) in [
            ("app:worker=expnad", "unknown tool"),
            ("app:worker=admin_audit", "an admin tool"),
            ("app:worker=tenant_list", "an operator tool"),
            ("escurel:worker=expand", "reserved"),
            ("app:worker=expand;app:worker=search", "twice"),
            ("app:worker", "not `role=tool"),
            ("=expand", "names no role"),
        ] {
            let err = RoleTools::parse(raw).unwrap_err();
            assert!(err.contains(needle), "{raw}: {err}");
        }
    }
}
