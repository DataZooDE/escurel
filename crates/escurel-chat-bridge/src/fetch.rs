//! Where the bridge may download an attachment from.
//!
//! An activity names its attachments' URLs, and the bridge downloads them and
//! ingests the bytes as the asker — so an unchecked URL is a server-side
//! request forgery: `http://127.0.0.1:8080/metrics` or a cloud metadata
//! address would become a page the asker can read. The rules:
//!
//! * the connector's own origin (the `serviceUrl` the channel token is bound
//!   to) is trusted as is, and is the only origin that receives the bot's
//!   bearer — compared as parsed scheme + host + port, never as a prefix;
//! * anything else must be `https`, on a configured host, and resolve only to
//!   public addresses; the request is pinned to the addresses that were
//!   checked (no second lookup to rebind), and redirects are not followed.

use std::net::{IpAddr, SocketAddr};

use reqwest::Url;

/// Where a download may go, decided before any request is sent.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// The connector: the bound `serviceUrl`'s origin. Gets the bearer.
    Connector,
    /// Another host, allowed: fetch it pinned to these checked addresses.
    Pinned {
        host: String,
        addrs: Vec<SocketAddr>,
    },
}

/// The download is refused before a request leaves.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refused {
    #[error("not a URL: {0}")]
    BadUrl(String),
    #[error("`{0}` is not https")]
    NotHttps(String),
    #[error("host `{0}` is not an allowed attachment host")]
    HostNotAllowed(String),
    #[error("host `{0}` does not resolve")]
    Unresolved(String),
    #[error("host `{host}` resolves to the non-public address {addr}")]
    NonPublic { host: String, addr: IpAddr },
}

/// Same scheme, host and port.
#[must_use]
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

/// `host` against the allow-list: an exact name, or `*.suffix` for any
/// subdomain of `suffix` (not `suffix` itself).
#[must_use]
pub fn host_allowed(host: &str, patterns: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p.trim().to_ascii_lowercase();
        match p.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1),
            None => host == p,
        }
    })
}

/// A globally routable unicast address: not loopback, private, link-local,
/// shared (CGNAT), unspecified, broadcast, multicast or documentation space.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local())
        }
    }
}

/// Decide where `url` may be fetched from, resolving its host.
///
/// # Errors
/// Why the download is refused; no request has been sent.
pub async fn check(url: &str, service_url: &str, hosts: &[String]) -> Result<Target, Refused> {
    let parsed = Url::parse(url).map_err(|e| Refused::BadUrl(e.to_string()))?;
    if let Ok(service) = Url::parse(service_url)
        && same_origin(&parsed, &service)
    {
        return Ok(Target::Connector);
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| Refused::BadUrl(url.to_owned()))?
        .to_owned();
    if !host_allowed(&host, hosts) {
        return Err(Refused::HostNotAllowed(host));
    }
    if parsed.scheme() != "https" {
        return Err(Refused::NotHttps(url.to_owned()));
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|_| Refused::Unresolved(host.clone()))?
        .collect();
    if addrs.is_empty() {
        return Err(Refused::Unresolved(host));
    }
    if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
        return Err(Refused::NonPublic {
            host,
            addr: bad.ip(),
        });
    }
    Ok(Target::Pinned { host, addrs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_lookalike_host_is_not_the_connectors_origin() {
        let service = url("https://smba.example");
        assert!(same_origin(&url("https://smba.example/v3/x"), &service));
        assert!(same_origin(&url("https://SMBA.example:443/a"), &service));
        assert!(!same_origin(
            &url("https://smba.example.evil.test/x"),
            &service
        ));
        assert!(!same_origin(&url("http://smba.example/x"), &service));
        assert!(!same_origin(&url("https://smba.example:8443/x"), &service));
    }

    #[test]
    fn wildcards_match_subdomains_only() {
        let hosts = vec!["*.sharepoint.com".to_owned(), "files.example".to_owned()];
        assert!(host_allowed("contoso.sharepoint.com", &hosts));
        assert!(host_allowed("a.b.SharePoint.com", &hosts));
        assert!(host_allowed("files.example", &hosts));
        assert!(!host_allowed("sharepoint.com", &hosts));
        assert!(!host_allowed("evilsharepoint.com", &hosts));
        assert!(!host_allowed("sharepoint.com.evil.test", &hosts));
        assert!(!host_allowed("x.files.example", &hosts));
    }

    #[test]
    fn internal_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["52.112.0.1", "2603:1000::1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn only_the_connector_skips_the_checks() {
        let hosts = vec!["localhost".to_owned()];
        assert_eq!(
            check("http://127.0.0.1:9/a", "http://127.0.0.1:9", &hosts).await,
            Ok(Target::Connector)
        );
        assert!(matches!(
            check("http://127.0.0.1:10/a", "http://127.0.0.1:9", &hosts).await,
            Err(Refused::HostNotAllowed(_))
        ));
        assert!(matches!(
            check("http://localhost:10/a", "http://127.0.0.1:9", &hosts).await,
            Err(Refused::NotHttps(_))
        ));
        assert!(matches!(
            check("https://localhost:10/a", "http://127.0.0.1:9", &hosts).await,
            Err(Refused::NonPublic { .. })
        ));
    }
}
