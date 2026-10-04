//! How a registered SQL-source credential becomes a connection string at ATTACH time.
//!
//! The credential registry stores either an inline secret (legacy, development only) or a
//! REFERENCE (`env:` / `gsm:` / `file:`) that is resolved when the source is attached, so a
//! connection string never has to live in the registry. The policy (which references a tenant may
//! name, which database hosts and files may be attached) belongs to the operator and lives in the
//! server; this crate only asks the installed [`CredentialResolver`].

use std::sync::Arc;

/// The operator's hooks for a credential, installed on an [`crate::Indexer`] by the server.
pub trait CredentialResolver: Send + Sync {
    /// The secret behind `raw` (what the registry stores): a reference is resolved, an inline value
    /// is returned as is. Every failure is a message that does not reveal which reference was wrong.
    fn resolve(&self, raw: &str) -> Result<String, String>;

    /// Whether the attach target in `resolved` (a DSN, or a database file path) is one the operator
    /// lets a tenant reach: host ranges for network databases, directories for file databases.
    fn check_target(&self, connector: &str, resolved: &str) -> Result<(), String>;
}

/// A shared resolver handle.
pub type SharedResolver = Arc<dyn CredentialResolver>;

/// Whether `raw` has the shape of a secret reference (`env:NAME`, `gsm:name`, `file:/path`).
#[must_use]
pub fn is_secret_reference(raw: &str) -> bool {
    ["env:", "gsm:", "file:"].iter().any(|p| {
        raw.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains(char::is_whitespace))
    })
}
