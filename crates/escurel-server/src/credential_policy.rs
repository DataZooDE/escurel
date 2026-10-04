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
//!   variable unset no file database can be attached at all.

use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;

use escurel_index::CredentialResolver;

use crate::egress::{EgressPolicy, is_public_ip};

/// The resolver the gateway installs on its indexers.
#[derive(Debug, Clone)]
pub struct ServerCredentialPolicy {
    policy: EgressPolicy,
}

impl ServerCredentialPolicy {
    #[must_use]
    pub fn new(policy: EgressPolicy) -> Self {
        Self { policy }
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

    fn check_network(&self, dsn: &str) -> Result<(), String> {
        let hosts = dsn_hosts(dsn);
        if hosts.is_empty() {
            return Err(
                "egress policy: the database connection string names no host (unix \
                        sockets and the default local server are not reachable)"
                    .to_owned(),
            );
        }
        for (host, port) in hosts {
            let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
                Ok(ip) => vec![ip],
                Err(_) => (host.as_str(), port)
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
            for ip in addrs {
                let ok = is_public_ip(ip) || (self.policy.allow_loopback && ip.is_loopback());
                if !ok {
                    return Err(
                        "egress policy: the database host is in a private, loopback or \
                         link-local range and is refused"
                            .to_owned(),
                    );
                }
            }
        }
        Ok(())
    }
}

impl CredentialResolver for ServerCredentialPolicy {
    fn resolve(&self, raw: &str) -> Result<String, String> {
        self.policy.secrets.resolve(raw)
    }

    fn check_target(&self, connector: &str, resolved: &str) -> Result<(), String> {
        match connector {
            "sqlite" => self.check_file(resolved),
            "postgres" | "mysql" => self.check_network(resolved),
            _ => Ok(()),
        }
    }
}

/// The `(host, port)` pairs a libpq / DuckDB connection string names: `key=value` pairs
/// (`host=`, `hostaddr=`, `port=`) or a `postgres://user:pw@host:port/db` URI.
#[must_use]
pub fn dsn_hosts(dsn: &str) -> Vec<(String, u16)> {
    let dsn = dsn.trim();
    for scheme in ["postgresql://", "postgres://", "mysql://"] {
        if dsn.starts_with(scheme) {
            return reqwest::Url::parse(dsn)
                .ok()
                .and_then(|u| {
                    let host = u.host_str()?.trim_matches(['[', ']']).to_owned();
                    Some(vec![(host, u.port().unwrap_or(5432))])
                })
                .unwrap_or_default();
        }
    }
    let mut host = None;
    let mut hostaddr = None;
    let mut port = 5432_u16;
    for tok in dsn.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        let v = v.trim_matches(['\'', '"']);
        match k {
            "host" => host = Some(v.to_owned()),
            "hostaddr" => hostaddr = Some(v.to_owned()),
            "port" => port = v.parse().unwrap_or(5432),
            _ => {}
        }
    }
    // `hostaddr` is what libpq actually connects to; judge it as well as the name.
    let mut out = Vec::new();
    if let Some(h) = hostaddr {
        out.push((h, port));
    }
    if let Some(h) = host {
        // A path is a unix socket directory: not a network host.
        if !h.starts_with('/') {
            out.push((h, port));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(dsn: &str) -> Vec<String> {
        dsn_hosts(dsn).into_iter().map(|(h, _)| h).collect()
    }

    #[test]
    fn a_dsn_names_its_hosts_in_both_libpq_forms() {
        assert_eq!(
            hosts("host=db.example.com port=5432 user=u password=p"),
            ["db.example.com"]
        );
        assert_eq!(
            hosts("postgresql://u:p@db.example.com:6543/shop"),
            ["db.example.com"]
        );
        assert_eq!(
            hosts("host=10.0.0.5 hostaddr=169.254.169.254"),
            ["169.254.169.254", "10.0.0.5"]
        );
        assert_eq!(hosts("host='[::1]' dbname=x"), ["[::1]"]);
    }

    #[test]
    fn a_unix_socket_or_no_host_names_no_network_host() {
        assert!(hosts("host=/var/run/postgresql dbname=x").is_empty());
        assert!(hosts("dbname=x user=u").is_empty());
    }

    #[test]
    fn private_hosts_are_refused_and_loopback_needs_the_flag() {
        let strict = ServerCredentialPolicy::new(EgressPolicy::default());
        for dsn in [
            "host=10.1.2.3 dbname=x",
            "host=169.254.169.254 dbname=x",
            "host=127.0.0.1 dbname=x",
            "postgres://u:p@192.168.0.4/x",
        ] {
            let e = strict.check_target("postgres", dsn).unwrap_err();
            assert!(e.contains("egress policy"), "{dsn}: {e}");
        }
        let dev = ServerCredentialPolicy::new(EgressPolicy {
            allow_loopback: true,
            ..EgressPolicy::default()
        });
        assert!(
            dev.check_target("postgres", "host=127.0.0.1 dbname=x")
                .is_ok()
        );
        assert!(
            dev.check_target("postgres", "host=10.1.2.3 dbname=x")
                .is_err()
        );
    }

    #[test]
    fn a_file_database_needs_an_operator_directory() {
        let none = ServerCredentialPolicy::new(EgressPolicy::default());
        assert!(
            none.check_target("sqlite", "/etc/hostname")
                .unwrap_err()
                .contains("not attachable")
        );
    }
}
