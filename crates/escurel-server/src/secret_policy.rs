//! What a tenant may NAME as a credential (`secret_ref`).
//!
//! A reference used to resolve ANY environment variable or ANY file of the gateway host and the
//! value was sent, as a bearer token, to whatever host an admin registered: a tenant admin could
//! exfiltrate `env:ESCUREL_*` operator secrets or `file:/etc/...`. The operator now decides what is
//! nameable:
//! - `gsm:NAME` — it only ever reads `ESCUREL_SECRET_<TENANT>__<NAME>`, which the substrate injects;
//! - `env:NAME` — only `ESCUREL_SECRET_<TENANT>__*` (the TENANT's own namespace), or a name the
//!   operator lists in `ESCUREL_SECRET_ENV_ALLOW` (comma list; `tenant:NAME` for one tenant, a bare
//!   `NAME` for every tenant — the operator's explicit choice);
//! - `file:/path` — only under `<dir>/<tenant>/` for a directory of `ESCUREL_SECRET_FILE_DIRS`
//!   (`:`-separated, default `/run/secrets`), after the path is canonicalised (no `..`, no symlink
//!   out of the directory, never `/proc`, `/sys` or `/dev`).
//!
//! The secret NAME after the prefix holds no `__` (the delimiter is single, so a variable can only ever
//! belong to one tenant), and an id whose token holds `__` (`a__b`, `a--b`) has no environment namespace.
//! `<TENANT>` is the tenant id upper-cased with every character that is not a letter or digit turned
//! into `_` (`stuttgart-ai` → `STUTTGART_AI`). The namespace used to be global (`ESCUREL_SECRET_*`),
//! so one tenant's admin could name another tenant's secret.
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

    /// Is `raw` (`env:` / `gsm:` / `file:`) something this gateway lets `tenant` name? Lexical only:
    /// it never touches the file system, so it is safe to call at registration.
    #[must_use]
    pub fn permits(&self, tenant: &str, raw: &str) -> bool {
        if let Some(name) = raw.strip_prefix("env:") {
            return !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && (tenant_env_prefix(tenant).is_some_and(|prefix| {
                    name.strip_prefix(&prefix)
                        .is_some_and(|rest| !rest.is_empty() && !rest.contains("__"))
                }) || self.env_allowed(tenant, name));
        }
        if let Some(name) = raw.strip_prefix("gsm:") {
            return !name.is_empty() && tenant_env_prefix(tenant).is_some();
        }
        if let Some(path) = raw.strip_prefix("file:") {
            let path = Path::new(path);
            let Some(sub) = tenant_dir_name(tenant) else {
                return false;
            };
            return is_plain_absolute(path)
                && !DENIED_ROOTS.iter().any(|d| path.starts_with(d))
                && self
                    .file_dirs
                    .iter()
                    .any(|d| path.starts_with(d.join(&sub)));
        }
        false
    }

    /// An operator-listed name: `NAME` for every tenant, `tenant:NAME` for one.
    fn env_allowed(&self, tenant: &str, name: &str) -> bool {
        self.env_names.iter().any(|n| match n.split_once(':') {
            Some((t, nm)) => t == tenant && nm == name,
            None => n == name,
        })
    }

    /// Whether `canonical` lives under `<dir>/<tenant>/` of a configured directory.
    fn contains_canonical(&self, tenant: &str, canonical: &Path) -> bool {
        let Some(sub) = tenant_dir_name(tenant) else {
            return false;
        };
        self.file_dirs.iter().any(|d| {
            d.join(&sub)
                .canonicalize()
                .map(|cd| canonical.starts_with(&cd))
                .unwrap_or(false)
        }) && !DENIED_ROOTS.iter().any(|d| canonical.starts_with(d))
    }

    /// Resolve a reference at CALL time. Every failure is the same message naming the reference.
    pub fn resolve(&self, tenant: &str, raw: &str) -> Result<String, String> {
        let unavailable = || format!("secret reference `{raw}` is not available");
        let non_empty = |v: Option<String>| {
            v.map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(unavailable)
        };
        if raw.starts_with("env:") || raw.starts_with("gsm:") || raw.starts_with("file:") {
            if !self.permits(tenant, raw) {
                return Err(unavailable());
            }
            if let Some(name) = raw.strip_prefix("env:") {
                return non_empty(std::env::var(name).ok());
            }
            if let Some(name) = raw.strip_prefix("gsm:") {
                let Some(prefix) = tenant_env_prefix(tenant) else {
                    return Err(unavailable());
                };
                let var = format!(
                    "{prefix}{}",
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
            if !self.contains_canonical(tenant, &canonical) {
                return Err(unavailable());
            }
            return non_empty(std::fs::read_to_string(canonical).ok());
        }
        // The legacy inline secret, kept only for development.
        Ok(raw.to_owned())
    }
}

/// `ESCUREL_SECRET_<TENANT>__`: the environment namespace of one tenant's secrets, `None` for an id
/// that cannot have one (its token holds the `__` delimiter: see [`escurel_admin::secret_env_namespace`]).
fn tenant_env_prefix(tenant: &str) -> Option<String> {
    escurel_admin::secret_env_namespace(tenant).map(|t| format!("{ENV_PREFIX}{t}__"))
}

/// The sub-directory of one tenant's secret files; `None` for an id that is not a plain name (so a
/// tenant id can never walk out of the secret directory).
fn tenant_dir_name(tenant: &str) -> Option<String> {
    (!tenant.is_empty()
        && tenant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| tenant.to_owned())
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

    const T: &str = "acme";

    fn policy(dir: &Path) -> SecretPolicy {
        SecretPolicy {
            file_dirs: vec![dir.to_path_buf()],
            env_names: vec!["MY_ALLOWED".to_owned(), "globex:ONLY_GLOBEX".to_owned()],
        }
    }

    #[test]
    fn a_tenant_names_its_own_env_namespace_allow_listed_names_and_nothing_else() {
        let p = policy(Path::new("/run/secrets"));
        assert!(p.permits(T, "env:ESCUREL_SECRET_ACME__CRM"));
        assert!(
            p.permits(T, "env:MY_ALLOWED"),
            "a bare name is for every tenant"
        );
        assert!(!p.permits(T, "env:HOME"));
        assert!(!p.permits(T, "env:escurel_secret_lower"));
        assert!(!p.permits(T, "env:"));
        // The old global namespace is gone, and so is another tenant's.
        assert!(!p.permits(T, "env:ESCUREL_SECRET_CRM"));
        assert!(!p.permits(T, "env:ESCUREL_SECRET_GLOBEX__CRM"));
        // `tenant:NAME` allows exactly that tenant.
        assert!(p.permits("globex", "env:ONLY_GLOBEX"));
        assert!(!p.permits(T, "env:ONLY_GLOBEX"));
        // A tenant id with punctuation maps to one namespace.
        assert!(p.permits("stuttgart-ai", "env:ESCUREL_SECRET_STUTTGART_AI__X"));
    }

    #[test]
    fn one_tenant_can_never_name_another_tenants_namespace() {
        let p = policy(Path::new("/run/secrets"));
        // `a__b` encodes to `A__B`, whose variables start with `ESCUREL_SECRET_A__`: tenant `a`'s own prefix.
        assert!(
            !p.permits("a", "env:ESCUREL_SECRET_A__B__TOKEN"),
            "tenant `a` named tenant `a__b`'s secret"
        );
        // ... and a tenant whose encoding holds the delimiter has no env namespace at all.
        assert!(!p.permits("a__b", "env:ESCUREL_SECRET_A__B__TOKEN"));
        assert!(!p.permits("a--b", "env:ESCUREL_SECRET_A__B__TOKEN"));
        assert!(p.resolve("a__b", "gsm:token").is_err());
        // An ordinary hyphenated id still has its namespace.
        assert!(p.permits("a-b", "env:ESCUREL_SECRET_A_B__TOKEN"));
    }

    #[test]
    fn files_must_sit_lexically_under_the_tenants_directory() {
        let p = policy(Path::new("/run/secrets"));
        assert!(p.permits(T, "file:/run/secrets/acme/crm"));
        assert!(!p.permits(T, "file:/run/secrets/crm"));
        assert!(!p.permits(T, "file:/run/secrets/globex/crm"));
        assert!(!p.permits(T, "file:/etc/hostname"));
        assert!(!p.permits(T, "file:/run/secrets/acme/../globex/crm"));
        assert!(!p.permits(T, "file:relative"));
        assert!(!p.permits(T, "file:/proc/self/environ"));
        // A tenant id that is not a plain name has no directory at all.
        assert!(!p.permits("../etc", "file:/run/secrets/../etc/x"));
    }

    #[test]
    fn a_symlink_out_of_the_tenants_directory_is_refused_with_the_generic_message() {
        let allowed = tempfile::tempdir().unwrap();
        let mine = allowed.path().join(T);
        std::fs::create_dir(&mine).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("real"), "stolen").unwrap();
        std::os::unix::fs::symlink(outside.path().join("real"), mine.join("link")).unwrap();
        std::fs::write(mine.join("ok"), " fine \n").unwrap();
        // Another tenant's file in the same secret directory.
        std::fs::create_dir(allowed.path().join("globex")).unwrap();
        std::fs::write(allowed.path().join("globex/theirs"), "theirs").unwrap();
        let p = policy(allowed.path());
        let link = format!("file:{}", mine.join("link").display());
        assert_eq!(
            p.resolve(T, &link).unwrap_err(),
            format!("secret reference `{link}` is not available")
        );
        assert_eq!(
            p.resolve(T, &format!("file:{}", mine.join("ok").display()))
                .unwrap(),
            "fine"
        );
        let theirs = format!("file:{}", allowed.path().join("globex/theirs").display());
        assert!(p.resolve(T, &theirs).is_err(), "another tenant's secret");
        assert_eq!(p.resolve("globex", &theirs).unwrap(), "theirs");
    }
}
