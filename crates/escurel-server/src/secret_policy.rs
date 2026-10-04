//! What a tenant may NAME as a credential (`secret_ref`).
//!
//! A reference used to resolve ANY environment variable or ANY file of the gateway host and the
//! value was sent, as a bearer token, to whatever host an admin registered: a tenant admin could
//! exfiltrate `env:ESCUREL_*` operator secrets or `file:/etc/...`. The operator now decides what is
//! nameable:
//! - `gsm:NAME` — always (it only ever reads `ESCUREL_SECRET_<NAME>`, which the substrate injects);
//! - `env:NAME` — only `ESCUREL_SECRET_*` or a name in `ESCUREL_SECRET_ENV_ALLOW` (comma list);
//! - `file:/path` — only under a directory of `ESCUREL_SECRET_FILE_DIRS` (`:`-separated, default
//!   `/run/secrets`), after the path is canonicalised (no `..`, no symlink out of the directory,
//!   never `/proc`, `/sys` or `/dev`).
//!
//! A reference that fails the policy or cannot be read answers with the SAME words, so it is not an
//! oracle for which files or variables exist.

use std::path::{Component, Path, PathBuf};

const ENV_PREFIX: &str = "ESCUREL_SECRET_";
const DENIED_ROOTS: [&str; 3] = ["/proc", "/sys", "/dev"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPolicy {
    pub file_dirs: Vec<PathBuf>,
    pub env_names: Vec<String>,
}

impl Default for SecretPolicy {
    fn default() -> Self {
        Self {
            file_dirs: vec![PathBuf::from("/run/secrets")],
            env_names: Vec::new(),
        }
    }
}

impl SecretPolicy {
    /// From `ESCUREL_SECRET_FILE_DIRS` / `ESCUREL_SECRET_ENV_ALLOW`; unset keeps the strict default.
    #[must_use]
    pub fn from_env() -> Self {
        let mut p = Self::default();
        if let Ok(v) = std::env::var("ESCUREL_SECRET_FILE_DIRS") {
            p.file_dirs = v
                .split(':')
                .filter(|s| !s.trim().is_empty())
                .map(|s| PathBuf::from(s.trim()))
                .collect();
        }
        if let Ok(v) = std::env::var("ESCUREL_SECRET_ENV_ALLOW") {
            p.env_names = v
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
        p
    }

    /// Is `raw` (`env:` / `gsm:` / `file:`) something this gateway lets a tenant name? Lexical only:
    /// it never touches the file system, so it is safe to call at registration.
    #[must_use]
    pub fn permits(&self, raw: &str) -> bool {
        if let Some(name) = raw.strip_prefix("env:") {
            return !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && (name.starts_with(ENV_PREFIX) || self.env_names.iter().any(|n| n == name));
        }
        if let Some(name) = raw.strip_prefix("gsm:") {
            return !name.is_empty();
        }
        if let Some(path) = raw.strip_prefix("file:") {
            let path = Path::new(path);
            return is_plain_absolute(path)
                && !DENIED_ROOTS.iter().any(|d| path.starts_with(d))
                && self.file_dirs.iter().any(|d| path.starts_with(d));
        }
        false
    }

    /// The directory `path` canonically lives under, if any configured directory contains it.
    fn contains_canonical(&self, canonical: &Path) -> bool {
        self.file_dirs.iter().any(|d| {
            d.canonicalize()
                .map(|cd| canonical.starts_with(&cd))
                .unwrap_or(false)
        }) && !DENIED_ROOTS.iter().any(|d| canonical.starts_with(d))
    }

    /// Resolve a reference at CALL time. Every failure is the same message naming the reference.
    pub fn resolve(&self, raw: &str) -> Result<String, String> {
        let unavailable = || format!("secret reference `{raw}` is not available");
        let non_empty = |v: Option<String>| {
            v.map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(unavailable)
        };
        if raw.starts_with("env:") || raw.starts_with("gsm:") || raw.starts_with("file:") {
            if !self.permits(raw) {
                return Err(unavailable());
            }
            if let Some(name) = raw.strip_prefix("env:") {
                return non_empty(std::env::var(name).ok());
            }
            if let Some(name) = raw.strip_prefix("gsm:") {
                let var = format!(
                    "{ENV_PREFIX}{}",
                    name.chars()
                        .map(|c| if c.is_ascii_alphanumeric() {
                            c.to_ascii_uppercase()
                        } else {
                            '_'
                        })
                        .collect::<String>()
                );
                return non_empty(std::env::var(var).ok());
            }
            let path = raw.strip_prefix("file:").unwrap_or_default();
            let canonical = Path::new(path).canonicalize().map_err(|_| unavailable())?;
            if !self.contains_canonical(&canonical) {
                return Err(unavailable());
            }
            return non_empty(std::fs::read_to_string(canonical).ok());
        }
        // The legacy inline secret, kept only for development.
        Ok(raw.to_owned())
    }
}

/// An absolute path made only of normal components (no `.` / `..`).
fn is_plain_absolute(p: &Path) -> bool {
    p.is_absolute()
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> SecretPolicy {
        SecretPolicy {
            file_dirs: vec![dir.to_path_buf()],
            env_names: vec!["MY_ALLOWED".to_owned()],
        }
    }

    #[test]
    fn only_named_env_prefixed_or_allow_listed_variables_resolve() {
        let p = policy(Path::new("/run/secrets"));
        assert!(p.permits("env:ESCUREL_SECRET_CRM"));
        assert!(p.permits("env:MY_ALLOWED"));
        assert!(!p.permits("env:HOME"));
        assert!(!p.permits("env:escurel_secret_lower"));
        assert!(!p.permits("env:"));
    }

    #[test]
    fn files_must_sit_lexically_under_an_allowed_directory() {
        let p = policy(Path::new("/run/secrets"));
        assert!(p.permits("file:/run/secrets/crm"));
        assert!(!p.permits("file:/etc/hostname"));
        assert!(!p.permits("file:/run/secrets/../../etc/hostname"));
        assert!(!p.permits("file:relative"));
        assert!(!p.permits("file:/proc/self/environ"));
    }

    #[test]
    fn a_symlink_out_of_the_allowed_directory_is_refused_with_the_generic_message() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("real"), "stolen").unwrap();
        std::os::unix::fs::symlink(outside.path().join("real"), allowed.path().join("link"))
            .unwrap();
        std::fs::write(allowed.path().join("ok"), " fine \n").unwrap();
        let p = policy(allowed.path());
        let link = format!("file:{}", allowed.path().join("link").display());
        assert_eq!(
            p.resolve(&link).unwrap_err(),
            format!("secret reference `{link}` is not available")
        );
        assert_eq!(
            p.resolve(&format!("file:{}", allowed.path().join("ok").display()))
                .unwrap(),
            "fine"
        );
    }
}
