//! Credential shapes, in one place (feature `redact`).
//!
//! Three consumers ask the same question — "is there a secret in this
//! text?" — and must not drift apart:
//!
//! - pack export refuses a page that carries one ([`pack_secret`],
//!   INV-SECRETFREE: fail-closed, so a deliberately small deterministic set);
//! - the gateway's per-run tool-call summary scrubs them ([`scrub`]);
//! - the runner's run transcript scrubs them before it leaves the runner
//!   ([`scrub`], plus the run's own bearer passed explicitly).
//!
//! [`scrub`] replaces each credential with [`REDACTED`] and keeps the rest of
//! the text, whitespace included. It covers: the value after `Bearer` /
//! `Basic`, a JWT (`eyJ…`, whole or cut), an `sk-…` style API key, a
//! `<secret-ish key>=value` / `:value` assignment, a DSN's inline
//! credentials, a PEM/PGP private-key block, and any caller-named literal
//! (a token value the caller holds).

use std::sync::LazyLock;

use regex::Regex;

/// What a scrubbed credential is replaced with.
pub const REDACTED: &str = "[redacted]";

/// The shortest caller-named literal [`scrub`] acts on.
pub const MIN_LITERAL_CHARS: usize = 8;

/// Key-name fragments whose value is a credential, whatever it looks like.
const SECRET_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "bearer",
    "cookie",
    "private_key",
];

/// Whether a key (a JSON field name, the left side of `k=v`) names a
/// credential.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEY_PARTS.iter().any(|p| k.contains(p))
}

/// A DSN carrying inline credentials (`scheme://user:pass@host/…`,
/// including the empty-password `user:@host` shape).
static DSN_WITH_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:[^/\s@]*@").expect("static regex compiles")
});

/// A PEM/PGP private-key block header, case-insensitive, with or without
/// the PGP `… BLOCK` suffix.
static PRIVATE_KEY_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----")
        .expect("static regex compiles")
});

/// A whole private-key block: the header through its END line, or through
/// the end of the text when the text was cut inside it.
static PRIVATE_KEY_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----.*?(-----END [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----|\z)",
    )
    .expect("static regex compiles")
});

/// A key-value connection-string credential (`Password=hunter2;`).
/// Restricted to `=`-style assignments so prose (`token: set this via the
/// registry`) does not trip the pack gate.
static KV_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(password|passwd|pwd)\s*=\s*[^\s;"']+"#).expect("static regex compiles")
});

/// An `sk-…` style API key (OpenAI, Anthropic `sk-ant-…`, and lookalikes).
static API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}").expect("static regex compiles"));

/// A credential that must not leave the node in a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSecret {
    DsnWithCredentials,
    PrivateKeyBlock,
    KvPassword,
}

/// The first pack-blocking credential in `content`, if any. Deliberately
/// small and deterministic: a false positive is a refused export an
/// operator can fix; a false negative is a leaked credential.
#[must_use]
pub fn pack_secret(content: &str) -> Option<PackSecret> {
    if DSN_WITH_CREDENTIALS.is_match(content) {
        Some(PackSecret::DsnWithCredentials)
    } else if PRIVATE_KEY_HEADER.is_match(content) {
        Some(PackSecret::PrivateKeyBlock)
    } else if KV_PASSWORD.is_match(content) {
        Some(PackSecret::KvPassword)
    } else {
        None
    }
}

/// `text` with every credential replaced by [`REDACTED`]; `also` names
/// literal secrets the caller holds (a run's own bearer), scrubbed first and
/// wherever they occur. Entries shorter than [`MIN_LITERAL_CHARS`] are
/// ignored: no real credential is that short, and scrubbing one would shred
/// ordinary words.
#[must_use]
pub fn scrub(text: &str, also: &[&str]) -> String {
    let mut s = text.to_owned();
    for secret in also
        .iter()
        .filter(|v| v.chars().count() >= MIN_LITERAL_CHARS)
    {
        if s.contains(secret) {
            s = s.replace(secret, REDACTED);
        }
    }
    let s = PRIVATE_KEY_BLOCK.replace_all(&s, REDACTED);
    let s = DSN_WITH_CREDENTIALS.replace_all(&s, |c: &regex::Captures<'_>| {
        // Keep the scheme so the text still says what it was.
        let m = &c[0];
        let scheme = m.split("://").next().unwrap_or("");
        format!("{scheme}://{REDACTED}@")
    });
    let s = KV_PASSWORD.replace_all(&s, |c: &regex::Captures<'_>| {
        format!("{}={REDACTED}", &c[1])
    });
    let s = API_KEY.replace_all(&s, REDACTED);
    scrub_words(&s)
}

/// Word-level credentials: `Bearer <x>`, a JWT, `token=<x>`. Whitespace is
/// kept as it was.
fn scrub_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // The previous word named a credential (`Bearer`, `token:`): this one is its value.
    let mut value_next = false;
    for piece in s.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let space = &piece[word.len()..];
        if word.is_empty() {
            out.push_str(space);
            continue;
        }
        let at_sep = word.find(['=', ':']);
        let scrubbed = if (value_next && word != REDACTED) || looks_like_jwt(word) {
            REDACTED.to_owned()
        } else if let Some(i) = at_sep
            && i > 0
            && is_secret_key(&word[..i])
            && i + 1 < word.len()
            && &word[i + 1..] != REDACTED
        {
            format!("{}{REDACTED}", &word[..=i])
        } else {
            word.to_owned()
        };
        let label = word
            .trim_start_matches(['"', '\'', '{', '('])
            .trim_end_matches([':', '=', '"', '\'']);
        value_next = matches!(label.to_ascii_lowercase().as_str(), "bearer" | "basic")
            || (label.len() < word.len() && word.ends_with([':', '=']) && is_secret_key(label));
        out.push_str(&scrubbed);
        out.push_str(space);
    }
    out
}

/// A JWT, or what is left of one after a cut: base64url `eyJ…` (the encoded
/// `{"`) long enough not to be a word.
fn looks_like_jwt(word: &str) -> bool {
    let w =
        word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-');
    w.starts_with("eyJ") && (w.matches('.').count() >= 2 || w.len() >= 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_values_jwts_and_api_keys_are_scrubbed_and_prose_stays() {
        let s = scrub(
            "call with Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.sig and \
             key sk-ant-api03-ABCDEFGHIJKLMNOPQRST then token=hunter2 done",
            &[],
        );
        for leaked in ["eyJhbGciOi", "sk-ant-api03", "hunter2"] {
            assert!(!s.contains(leaked), "{leaked} in {s}");
        }
        assert!(s.starts_with("call with Bearer [redacted] and key"), "{s}");
        assert!(s.ends_with("token=[redacted] done"), "{s}");
    }

    #[test]
    fn a_cut_jwt_is_still_recognised() {
        let s = scrub("header eyJhbGciOiJSUzI1NiIsInR5", &[]);
        assert_eq!(s, "header [redacted]");
    }

    #[test]
    fn a_caller_named_literal_is_scrubbed_wherever_it_occurs() {
        let s = scrub(
            "got opaque-run-token-123 back (opaque-run-token-123)",
            &["opaque-run-token-123", "", "got"],
        );
        assert_eq!(s, "got [redacted] back ([redacted])");
    }

    #[test]
    fn dsn_credentials_key_blocks_and_kv_passwords_are_scrubbed() {
        let s = scrub(
            "postgres://app:s3cret@db/x and Password=hunter2; \
             -----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY----- tail",
            &[],
        );
        for leaked in ["s3cret", "hunter2", "MIIEow"] {
            assert!(!s.contains(leaked), "{leaked} in {s}");
        }
        assert!(s.contains("postgres://[redacted]@db/x"), "{s}");
        assert!(s.ends_with("tail"), "{s}");
    }

    #[test]
    fn ordinary_text_is_untouched() {
        let t = "updated markdown/instances/note/n1.md: 3 fields, see https://example.com/a?b=c";
        assert_eq!(scrub(t, &[]), t);
    }

    #[test]
    fn the_pack_gate_names_the_first_secret_kind() {
        assert_eq!(
            pack_secret("x postgres://u:p@h/db"),
            Some(PackSecret::DsnWithCredentials)
        );
        assert_eq!(
            pack_secret("-----BEGIN PGP PRIVATE KEY BLOCK-----"),
            Some(PackSecret::PrivateKeyBlock)
        );
        assert_eq!(pack_secret("Pwd = abc"), Some(PackSecret::KvPassword));
        assert_eq!(pack_secret("token: set this via the registry"), None);
    }
}
