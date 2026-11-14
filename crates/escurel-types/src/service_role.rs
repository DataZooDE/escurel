//! The built-in service roles (#828): a credential a service holds for
//! itself, with a narrow authority of its own. Here rather than in
//! `escurel-auth` so the verifier that reads them and the ACL that decides on
//! them name the same type.

/// A built-in service role: a credential a service holds for itself, with a
/// narrow authority of its own rather than a tool ceiling over a member's
/// (`ESCUREL_AUTH_ROLE_TOOLS` can only take away; these grant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRole {
    /// `escurel:runner`: claims inbox events, files the runner's lifecycle
    /// records, registers its `runtime` page and mints per-run tokens.
    Runner,
    /// `escurel:bridge`: a chat bridge's service credential.
    Bridge,
}
