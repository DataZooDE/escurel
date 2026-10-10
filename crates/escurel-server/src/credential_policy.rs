//! The operator's policy over SQL-source credentials: which references resolve, and which database
//! hosts / files a tenant admin may point a `sql_view` at.
//!
//! A credential's secret is a connection string, so "register a credential" is "make the gateway
//! open a connection to a place of the tenant's choosing". Three guards decide what that may mean:
//! - the secret is a REFERENCE (`env:`/`gsm:`/`file:`) resolved when the source is attached, under the
//!   same [`crate::secret_policy`] as endpoint credentials;
//! - a network database (`postgres`, `mysql`) must be reachable under the egress policy: every address
//!   its host resolves to has to be public (loopback only with `ESCUREL_EGRESS_ALLOW_LOOPBACK`);
//! - a file database (`sqlite`) must live under `ESCUREL_SQL_FILE_DIRS`, canonicalised; with the
//!   variable unset no file database can be attached at all. The directory connectors (`json_dir`,
//!   `parquet_dir`) read any file the gateway can, so they are confined to the same directories;
//! - the connection string is read with the driver's own grammar, only a short list of keys is
//!   accepted, and a host named by NAME is pinned to the address that was checked.

use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Component, Path};

use escurel_index::{CredentialResolver, dsn};

use crate::egress::{EgressPolicy, is_public_ip};

/// The resolver the gateway installs on its indexers.
#[derive(Debug, Clone)]
pub struct ServerCredentialPolicy {
    policy: EgressPolicy,
    /// The tenant this indexer serves: the secrets it may name are its own (see
    /// [`crate::secret_policy`]).
    tenant: String,
}

impl ServerCredentialPolicy {
    /// The policy for ONE tenant's indexer: the secrets it may name are that tenant's own.
    #[must_use]
    pub fn new(policy: EgressPolicy, tenant: &str) -> Self {
        Self {
            policy,
            tenant: tenant.to_owned(),
        }
    }

    fn check_file(&self, path: &str) -> Result<(), String> {
        if self.policy.sql_file_dirs.is_empty() {
            return Err(
                "egress policy: file databases are not attachable on this gateway \
                        (the operator has not set ESCUREL_SQL_FILE_DIRS)"
                    .to_owned(),
            );
        }
        let canonical = Path::new(path.trim()).canonicalize().map_err(|_| {
            "egress policy: the database file is not available to this gateway".to_owned()
        })?;
        let inside = self.policy.sql_file_dirs.iter().any(|d| {
            d.canonicalize()
                .map(|cd| canonical.starts_with(cd))
                .unwrap_or(false)
        });
        if inside {
            Ok(())
        } else {
            Err(
                "egress policy: the database file is outside the directories the operator \
                 exposes (ESCUREL_SQL_FILE_DIRS)"
                    .to_owned(),
            )
        }
    }

    /// A directory connector's glob must name a directory under `ESCUREL_SQL_FILE_DIRS`: absolute, no
    /// `..`, the directory before the first wildcard canonicalised and inside, and no symlink below it
    /// (as far as the pattern can reach) that leads outside. Unset = no directory connector at all.
    /// The words never name a file or say what is there.
    fn check_glob(&self, glob: &str) -> Result<(), String> {
        let refuse = || {
            "egress policy: the directory is outside the directories the operator exposes \
             (ESCUREL_SQL_FILE_DIRS)"
                .to_owned()
        };
        if self.policy.sql_file_dirs.is_empty() {
            return Err(
                "egress policy: directory sources are not available on this gateway \
                 (the operator has not set ESCUREL_SQL_FILE_DIRS)"
                    .to_owned(),
            );
        }
        let allowed: Vec<std::path::PathBuf> = self
            .policy
            .sql_file_dirs
            .iter()
            .filter_map(|d| d.canonicalize().ok())
            .collect();
        let path = Path::new(glob.trim());
        if !path.is_absolute() {
            return Err(refuse());
        }
        let is_meta = |c: &str| c.contains(['*', '?', '[', '{']);
        let mut base = std::path::PathBuf::new();
        let mut rest = 0usize;
        let mut recursive = false;
        let mut in_pattern = false;
        for c in path.components() {
            match c {
                Component::ParentDir | Component::CurDir | Component::Prefix(_) => {
                    return Err(refuse());
                }
                Component::RootDir => base.push("/"),
                Component::Normal(n) => {
                    let n = n.to_string_lossy();
                    if in_pattern || is_meta(&n) {
                        in_pattern = true;
                        rest += 1;
                        recursive |= n.contains("**");
                    } else {
                        base.push(&*n);
                    }
                }
            }
        }
        let base = base.canonicalize().map_err(|_| refuse())?;
        if !allowed.iter().any(|d| base.starts_with(d)) {
            return Err(refuse());
        }
        // The last component is the file pattern; the directories above it are what a symlink could
        // redirect. `**` can reach any depth: bound it.
        let depth = if recursive { 8 } else { rest.saturating_sub(1) };
        let mut budget = 20_000_usize;
        if links_escape(&base, depth, &allowed, &mut budget) {
            return Err(refuse());
        }
        Ok(())
    }

    /// Judge a network DSN and return it with the addresses pinned.
    ///
    /// The connection string is parsed the way the driver parses it ([`escurel_index::dsn`]), every
    /// key must be on the allow-list, and EVERY host and `hostaddr` it names must resolve only to
    /// public addresses (loopback only with `ESCUREL_EGRESS_ALLOW_LOOPBACK`). A Postgres DSN that
    /// names hosts by NAME gets a `hostaddr` appended with the very addresses that were checked, so
    /// libpq connects to what was judged and a DNS answer that changes between the check and the
    /// connection (rebinding) buys nothing; `host` stays for TLS verification.
    fn pin_network(&self, connector: &str, dsn: &str) -> Result<String, String> {
        let (allowed, default_port) = if connector == "mysql" {
            (dsn::MYSQL_KEYS, 3306)
        } else {
            (dsn::POSTGRES_KEYS, 5432)
        };
        let pairs = dsn::parse_pairs(dsn).map_err(|e| format!("egress policy: {e}"))?;
        for (i, (k, _)) in pairs.iter().enumerate() {
            if !allowed.contains(&k.as_str()) {
                return Err(format!(
                    "egress policy: the connection string key `{}` is not allowed here",
                    k.chars().take(32).collect::<String>()
                ));
            }
            if pairs[..i].iter().any(|(p, _)| p == k) {
                return Err(format!(
                    "egress policy: the connection string repeats `{k}`; name each key once"
                ));
            }
        }
        let targets = dsn::targets(&pairs, default_port).map_err(|_| {
            "egress policy: unix sockets are not reachable (name a network host)".to_owned()
        })?;
        if targets.is_empty() {
            return Err(
                "egress policy: the database connection string names no host (unix \
                        sockets and the default local server are not reachable)"
                    .to_owned(),
            );
        }
        // host name -> the first address it resolved to, in the order the `host` list names them.
        let mut pinned: Vec<(String, IpAddr)> = Vec::new();
        for t in &targets {
            let addrs: Vec<IpAddr> = match t.host.parse::<IpAddr>() {
                Ok(ip) => vec![ip],
                Err(_) => (t.host.as_str(), t.port)
                    .to_socket_addrs()
                    .map_err(|_| {
                        "egress policy: the database host could not be resolved".to_owned()
                    })?
                    .map(|a| a.ip())
                    .collect(),
            };
            if addrs.is_empty() {
                return Err("egress policy: the database host did not resolve".to_owned());
            }
            for ip in &addrs {
                let ok = is_public_ip(*ip) || (self.policy.allow_loopback && ip.is_loopback());
                if !ok {
                    return Err(
                        "egress policy: the database host is in a private, loopback or \
                         link-local range and is refused"
                            .to_owned(),
                    );
                }
            }
            pinned.push((t.host.clone(), addrs[0]));
        }
        let has = |k: &str| pairs.iter().any(|(p, _)| p == k);
        if connector == "postgres" && has("host") && !has("hostaddr") {
            let list = pairs
                .iter()
                .find(|(k, _)| k == "host")
                .map(|(_, v)| v.as_str())
                .unwrap_or_default();
            let addrs: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(|h| {
                    let h = h.trim_matches(['[', ']']);
                    pinned
                        .iter()
                        .find(|(n, _)| n == h)
                        .map_or_else(|| h.to_owned(), |(_, ip)| ip.to_string())
                })
                .collect();
            return Ok(dsn::with_param(dsn, "hostaddr", &addrs.join(",")));
        }
        Ok(dsn.to_owned())
    }
}

impl CredentialResolver for ServerCredentialPolicy {
    fn resolve(&self, raw: &str) -> Result<String, String> {
        self.policy.secrets.resolve(&self.tenant, raw)
    }

    fn check_target(&self, connector: &str, resolved: &str) -> Result<(), String> {
        self.pin_target(connector, resolved).map(|_| ())
    }

    fn check_directory(&self, _connector: &str, glob: &str) -> Result<(), String> {
        self.check_glob(glob)
    }

    fn pin_target(&self, connector: &str, resolved: &str) -> Result<String, String> {
        match connector {
            "sqlite" => self.check_file(resolved).map(|()| resolved.to_owned()),
            "postgres" | "mysql" => self.pin_network(connector, resolved),
            _ => Ok(resolved.to_owned()),
        }
    }
}

/// Does anything under `dir` (to `depth` levels, without following links) link OUTSIDE `allowed`? A
/// directory too large to look through counts as escaping: failing closed beats an unbounded walk.
fn links_escape(
    dir: &Path,
    depth: usize,
    allowed: &[std::path::PathBuf],
    budget: &mut usize,
) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // Nothing there, so nothing links anywhere.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        // A directory that cannot be listed may hold any link at all: fail CLOSED, as the doc says.
        Err(_) => return true,
    };
    for e in entries {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let Ok(e) = e else {
            return true;
        };
        let path = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return true;
        };
        if meta.file_type().is_symlink() {
            match path.canonicalize() {
                Ok(target) if allowed.iter().any(|d| target.starts_with(d)) => {}
                _ => return true,
            }
        } else if meta.is_dir() && depth > 0 && links_escape(&path, depth - 1, allowed, budget) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_that_cannot_be_listed_counts_as_escaping() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let hidden = root.path().join("hidden");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
        let allowed = [root.path().canonicalize().unwrap()];
        let mut budget = 100;
        let escaped = links_escape(root.path(), 4, &allowed, &mut budget);
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).unwrap();
        // As root the directory is listable anyway: nothing to assert.
        if running_as_root() {
            return;
        }
        assert!(
            escaped,
            "an unreadable directory may hide a link out: fail closed"
        );
        // A missing directory has nothing to link anywhere.
        let mut budget = 100;
        assert!(!links_escape(
            &root.path().join("absent"),
            4,
            &allowed,
            &mut budget
        ));
    }

    fn running_as_root() -> bool {
        std::fs::read_to_string("/proc/self/status")
            .map(|s| {
                s.lines()
                    .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
            })
            .unwrap_or(false)
    }

    fn policy(f: impl FnOnce(&mut EgressPolicy)) -> ServerCredentialPolicy {
        let mut p = EgressPolicy::default();
        f(&mut p);
        ServerCredentialPolicy::new(p, "acme")
    }

    #[test]
    fn private_hosts_are_refused_and_loopback_needs_the_flag() {
        let strict = policy(|_| {});
        for dsn in [
            "host=10.1.2.3 dbname=x",
            "host=169.254.169.254 dbname=x",
            "host=127.0.0.1 dbname=x",
            "postgres://u:p@192.168.0.4/x",
        ] {
            let e = strict.check_target("postgres", dsn).unwrap_err();
            assert!(e.contains("egress policy"), "{dsn}: {e}");
        }
        let dev = policy(|p| p.allow_loopback = true);
        assert!(
            dev.check_target("postgres", "host=127.0.0.1 dbname=x")
                .is_ok()
        );
        assert!(
            dev.check_target("postgres", "host=10.1.2.3 dbname=x")
                .is_err()
        );
    }

    /// The two spellings the review used to walk a private host past the check.
    #[test]
    fn libpq_spellings_that_hid_a_second_host_are_judged_too() {
        let strict = policy(|_| {});
        for dsn in [
            // spaces around `=`: the old split judged only the first `host`
            "host=93.184.216.34 host = 127.0.0.1 dbname=x",
            // the URI form's `hostaddr=` query parameter
            "postgresql://u:p@93.184.216.34/db?hostaddr=127.0.0.1&options=-cdatestyle%3Diso",
            // a host LIST: any private member is enough
            "host=93.184.216.34,10.0.0.5 dbname=x",
            "postgres://u@93.184.216.34:5432,127.0.0.1:5432/db",
            // a unix socket among the hosts is local, not a network host
            "host=/var/run/postgresql,93.184.216.34 dbname=x",
            // quoted values
            "host='127.0.0.1' dbname=x",
        ] {
            let e = strict.check_target("postgres", dsn).unwrap_err();
            assert!(e.contains("egress policy"), "{dsn}: {e}");
        }
    }

    #[test]
    fn keys_that_make_libpq_read_files_or_ask_another_service_are_refused() {
        let strict = policy(|_| {});
        for dsn in [
            "service=prod dbname=x",
            "host=93.184.216.34 passfile=/etc/passwd",
            "host=93.184.216.34 sslrootcert=/etc/shadow",
            "postgres://u@93.184.216.34/db?sslkey=/root/.ssh/id_rsa",
            "host=93.184.216.34 host=127.0.0.1",
        ] {
            let e = strict.check_target("postgres", dsn).unwrap_err();
            assert!(e.contains("egress policy"), "{dsn}: {e}");
        }
        // MySQL: no `socket`.
        assert!(
            strict
                .check_target("mysql", "host=93.184.216.34 socket=/var/run/mysqld.sock")
                .is_err()
        );
    }

    #[test]
    fn a_checked_host_name_is_pinned_to_the_address_that_was_checked() {
        let dev = policy(|p| p.allow_loopback = true);
        let pinned = dev
            .pin_target("postgres", "host=localhost dbname=x user=u")
            .unwrap();
        let pairs = dsn::parse_pairs(&pinned).unwrap();
        let addr = pairs
            .iter()
            .find(|(k, _)| k == "hostaddr")
            .map(|(_, v)| v.as_str())
            .expect("hostaddr pinned");
        assert!(addr == "127.0.0.1" || addr == "::1", "{pinned}");
        assert!(pairs.iter().any(|(k, v)| k == "host" && v == "localhost"));
        // The URI spelling gets a query parameter, not a malformed string.
        let uri = dev
            .pin_target("postgres", "postgresql://u:p@localhost/db")
            .unwrap();
        assert!(uri.contains("hostaddr="), "{uri}");
        // A literal address is pinned to itself.
        let lit = dev
            .pin_target("postgres", "host=127.0.0.1 dbname=x")
            .unwrap();
        assert!(lit.ends_with("hostaddr=127.0.0.1"), "{lit}");
    }

    #[test]
    fn a_file_database_needs_an_operator_directory() {
        let none = policy(|_| {});
        assert!(
            none.check_target("sqlite", "/etc/hostname")
                .unwrap_err()
                .contains("not attachable")
        );
    }

    #[test]
    fn a_directory_connector_stays_inside_the_operator_directories() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("data")).unwrap();
        std::fs::write(root.path().join("data/a.json"), "{}").unwrap();
        std::fs::write(outside.path().join("secret.json"), "{}").unwrap();
        let p = policy(|p| p.sql_file_dirs = vec![root.path().join("data")]);
        let d = root.path().join("data");
        for ok in [
            format!("{}/*.json", d.display()),
            format!("{}/**/*.json", d.display()),
        ] {
            assert!(p.check_directory("json_dir", &ok).is_ok(), "{ok}");
        }
        for bad in [
            format!("{}/*.json", outside.path().display()),
            "/etc/passw*".to_owned(),
            "/proc/self/environ*".to_owned(),
            format!("{}/../*.json", d.display()),
            "relative/*.json".to_owned(),
            format!("{}/*.json", root.path().display()),
        ] {
            let e = p.check_directory("json_dir", &bad).unwrap_err();
            assert!(e.contains("egress policy"), "{bad}: {e}");
            assert!(!e.contains("secret.json") && !e.contains("passw"), "{e}");
        }
        // A link inside the exposed directory that leads out is an escape.
        std::os::unix::fs::symlink(outside.path(), d.join("out")).unwrap();
        assert!(
            p.check_directory("json_dir", &format!("{}/**/*.json", d.display()))
                .is_err()
        );
        // Unset = no directory connector at all.
        let none = policy(|_| {});
        assert!(
            none.check_directory("json_dir", &format!("{}/*.json", d.display()))
                .unwrap_err()
                .contains("not available")
        );
    }
}
