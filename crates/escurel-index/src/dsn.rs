//! Connection strings, read the way the client library reads them.
//!
//! An egress policy that judges a DSN by a different grammar than the one the driver uses is not a
//! policy: libpq accepts `host = x` (spaces around `=`), `host='x y'`, several hosts (`host=a,b`),
//! `hostaddr=` and `service=`, and in the URI form every one of those again as a query parameter
//! (`?hostaddr=127.0.0.1`). The first version of the check split on whitespace and `=` and so judged
//! `host=example.com host = 127.0.0.1` by its first host only. This module parses BOTH spellings into
//! one list of `(key, value)` pairs, so the policy sees exactly what the driver will connect with.
//!
//! The parser follows libpq's `conninfo_parse` / `conninfo_uri_parse`: keys are `[^\s=]+`, values are
//! unquoted (ended by whitespace, `\` escapes the next character) or single-quoted (`\'` and `\\`
//! escapes); a URI is `scheme://[user[:password]@]host[:port][,host[:port]…][/dbname][?k=v&…]` with
//! percent-encoding throughout.

/// The pairs of a connection string, in order, both spellings normalised. `Err` carries a message that
/// never includes the string (it may hold a password).
pub fn parse_pairs(dsn: &str) -> Result<Vec<(String, String)>, String> {
    let dsn = dsn.trim();
    for scheme in ["postgresql://", "postgres://", "mysql://"] {
        if let Some(rest) = dsn.strip_prefix(scheme) {
            return parse_uri(rest);
        }
    }
    parse_key_value(dsn)
}

fn parse_key_value(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    loop {
        while it.peek().is_some_and(|c| c.is_whitespace()) {
            it.next();
        }
        if it.peek().is_none() {
            return Ok(out);
        }
        let mut key = String::new();
        while let Some(&c) = it.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            it.next();
        }
        while it.peek().is_some_and(|c| c.is_whitespace()) {
            it.next();
        }
        if it.next() != Some('=') || key.is_empty() {
            return Err("malformed connection string (a key without `=`)".to_owned());
        }
        while it.peek().is_some_and(|c| c.is_whitespace()) {
            it.next();
        }
        let mut value = String::new();
        if it.peek() == Some(&'\'') {
            it.next();
            loop {
                match it.next() {
                    None => return Err("malformed connection string (unterminated quote)".into()),
                    Some('\'') => break,
                    Some('\\') => match it.next() {
                        Some(c) => value.push(c),
                        None => return Err("malformed connection string".into()),
                    },
                    Some(c) => value.push(c),
                }
            }
        } else {
            while let Some(&c) = it.peek() {
                if c.is_whitespace() {
                    break;
                }
                it.next();
                if c == '\\' {
                    match it.next() {
                        Some(e) => value.push(e),
                        None => return Err("malformed connection string".into()),
                    }
                } else {
                    value.push(c);
                }
            }
        }
        out.push((key, value));
    }
}

fn percent_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| "malformed connection string (bad percent-encoding)".to_owned())?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "malformed connection string (not UTF-8)".to_owned())
}

fn parse_uri(rest: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let (before_query, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let (authority, dbname) = match before_query.split_once('/') {
        Some((a, d)) => (a, Some(d)),
        None => (before_query, None),
    };
    // userinfo ends at the LAST `@` of the authority.
    let (userinfo, hostspec) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    if let Some(u) = userinfo {
        match u.split_once(':') {
            Some((user, pw)) => {
                out.push(("user".to_owned(), percent_decode(user)?));
                out.push(("password".to_owned(), percent_decode(pw)?));
            }
            None => out.push(("user".to_owned(), percent_decode(u)?)),
        }
    }
    let mut hosts = Vec::new();
    let mut ports = Vec::new();
    for h in hostspec.split(',').filter(|h| !h.is_empty()) {
        let (host, port) = if let Some(v6) = h.strip_prefix('[') {
            let (addr, after) = v6
                .split_once(']')
                .ok_or_else(|| "malformed connection string (bad IPv6 host)".to_owned())?;
            (addr, after.strip_prefix(':'))
        } else {
            match h.rsplit_once(':') {
                Some((a, p)) => (a, Some(p)),
                None => (h, None),
            }
        };
        hosts.push(percent_decode(host)?);
        ports.push(port.unwrap_or("").to_owned());
    }
    if !hosts.is_empty() {
        out.push(("host".to_owned(), hosts.join(",")));
    }
    if ports.iter().any(|p| !p.is_empty()) {
        out.push(("port".to_owned(), ports.join(",")));
    }
    if let Some(d) = dbname.filter(|d| !d.is_empty()) {
        out.push(("dbname".to_owned(), percent_decode(d)?));
    }
    if let Some(q) = query {
        for kv in q.split('&').filter(|kv| !kv.is_empty()) {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| "malformed connection string (query parameter)".to_owned())?;
            out.push((percent_decode(k)?, percent_decode(v)?));
        }
    }
    Ok(out)
}

/// The keys a Postgres attach may carry. Everything else is refused, and notably the ones that make
/// libpq read a FILE or ask another service where to connect (`service`, `passfile`, `sslrootcert`,
/// `sslcert`, `sslkey`, `sslcrl`, `krbsrvname`, `gsslib`, `ssl_min_protocol_version` …) or run a
/// command (`sslpassword` callbacks, `requirepeer`).
pub const POSTGRES_KEYS: &[&str] = &[
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "sslmode",
    "options",
    "application_name",
    "connect_timeout",
];

/// The keys a MySQL attach may carry (no `socket`, no `ssl_ca`-style file keys).
pub const MYSQL_KEYS: &[&str] = &[
    "host",
    "port",
    "user",
    "passwd",
    "password",
    "database",
    "db",
    "ssl_mode",
    "connect_timeout",
];

/// A network target a connection string names: the host (or literal address) and its port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

/// Every network target in `pairs`: each entry of `host` AND of `hostaddr` (comma lists, ports lined
/// up by position, a single port applying to all). `Err` on a unix-socket path: that is a local
/// service, never a network host.
pub fn targets(pairs: &[(String, String)], default_port: u16) -> Result<Vec<Target>, String> {
    let ports: Vec<u16> = pairs
        .iter()
        .rev()
        .find(|(k, _)| k == "port")
        .map(|(_, v)| {
            v.split(',')
                .map(|p| p.trim().parse().unwrap_or(default_port))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    for key in ["hostaddr", "host"] {
        for (_, v) in pairs.iter().filter(|(k, _)| k == key) {
            for (i, h) in v.split(',').enumerate() {
                let h = h.trim();
                if h.is_empty() {
                    continue;
                }
                if h.starts_with('/') || h.starts_with('@') {
                    return Err("a unix socket is not a network host".to_owned());
                }
                let port = match ports.len() {
                    0 => default_port,
                    1 => ports[0],
                    _ => ports.get(i).copied().unwrap_or(default_port),
                };
                out.push(Target {
                    host: h.trim_matches(['[', ']']).to_owned(),
                    port,
                });
            }
        }
    }
    Ok(out)
}

/// `dsn` with `key=value` appended in the spelling `dsn` already uses: a bare ` key=value` for the
/// key/value form, a correctly percent-encoded query parameter for a URI (libpq refuses a literal `=`
/// inside a URI query value: "extra key/value separator").
#[must_use]
pub fn with_param(dsn: &str, key: &str, value: &str) -> String {
    let dsn = dsn.trim();
    let is_uri = ["postgresql://", "postgres://", "mysql://"]
        .iter()
        .any(|s| dsn.starts_with(s));
    if is_uri {
        let enc: String = value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b',' | b':' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        let sep = if dsn.contains('?') { '&' } else { '?' };
        format!("{dsn}{sep}{key}={enc}")
    } else {
        format!("{dsn} {key}={value}")
    }
}

/// `msg` with everything secret in `dsn` removed: the whole string, and the password (an error from
/// the driver can quote either the string or the value alone). One line, bounded.
#[must_use]
pub fn scrub(msg: &str, dsn: &str) -> String {
    let mut out = msg.replace(dsn, "***");
    if let Ok(pairs) = parse_pairs(dsn) {
        for (k, v) in pairs {
            if matches!(k.as_str(), "password" | "passwd") && !v.is_empty() {
                out = out.replace(&v, "***");
            }
        }
    }
    out.replace(['\n', '\r'], " ").chars().take(240).collect()
}

/// Whether the connection string sets `key`.
#[must_use]
pub fn has_key(dsn: &str, key: &str) -> bool {
    parse_pairs(dsn).is_ok_and(|p| p.iter().any(|(k, _)| k == key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(dsn: &str) -> Vec<String> {
        targets(&parse_pairs(dsn).unwrap(), 5432)
            .unwrap()
            .into_iter()
            .map(|t| t.host)
            .collect()
    }

    #[test]
    fn spaces_around_equals_and_quotes_are_read_like_libpq() {
        assert_eq!(
            hosts("host=example.com host = 127.0.0.1 dbname=x"),
            ["example.com", "127.0.0.1"]
        );
        assert_eq!(
            hosts("host = 'db.example.com' port='5433'"),
            ["db.example.com"]
        );
        assert_eq!(hosts("host=a,b port=1,2"), ["a", "b"]);
    }

    #[test]
    fn uri_query_parameters_name_hosts_too() {
        assert_eq!(
            hosts("postgresql://u:p@example.com/db?hostaddr=127.0.0.1&options=-cdatestyle%3Diso"),
            ["127.0.0.1", "example.com"]
        );
        assert_eq!(hosts("postgres://u@a:1,b:2/db"), ["a", "b"]);
        assert_eq!(hosts("postgres://u@[::1]:5433/db"), ["::1"]);
        assert_eq!(hosts("postgres://u:p%40ss@h/db?host=evil"), ["h", "evil"]);
    }

    #[test]
    fn a_unix_socket_among_the_hosts_is_an_error() {
        assert!(targets(&parse_pairs("host=/var/run,10.0.0.1").unwrap(), 5432).is_err());
    }

    #[test]
    fn malformed_input_is_an_error_that_does_not_echo_the_string() {
        let e = parse_pairs("host=x password").unwrap_err();
        assert!(!e.contains("password"), "{e}");
        assert!(parse_pairs("host='unterminated").is_err());
    }

    #[test]
    fn a_param_is_appended_in_the_spelling_the_dsn_uses() {
        assert_eq!(
            with_param("host=h dbname=d", "options", "-cstatement_timeout=5"),
            "host=h dbname=d options=-cstatement_timeout=5"
        );
        assert_eq!(
            with_param("postgres://u@h/d", "options", "-cstatement_timeout=5"),
            "postgres://u@h/d?options=-cstatement_timeout%3D5"
        );
        assert_eq!(
            with_param("postgres://u@h/d?sslmode=disable", "hostaddr", "1.2.3.4"),
            "postgres://u@h/d?sslmode=disable&hostaddr=1.2.3.4"
        );
    }

    #[test]
    fn an_existing_option_is_found_by_key_not_by_substring() {
        assert!(has_key("host=h options=-c", "options"));
        assert!(!has_key("host=h application_name=options", "options"));
        assert!(has_key("postgres://u@h/d?options=-c", "options"));
    }
}
