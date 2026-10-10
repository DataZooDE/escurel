//! Opaque list-cursor codec shared by the paged list surfaces:
//! `<base64url("<sort-key-or-empty>|<row-id>")>.<base64url(HMAC tag)>`. Every cursor family
//! (instances, events, chat, rows, drafts) goes through [`seal`] / [`unseal`], so a token the server
//! did not issue — made up, or edited — is refused as `invalid_cursor` instead of being decoded.
//! The tag also covers a SCOPE (`<family>:<tenant>:<what is being paged>`): a cursor replayed on
//! another tenant, skill or list answers `invalid_cursor` too. The sort key is stored
//! at FULL microsecond precision (`%Y-%m-%d %H:%M:%S.%f`) so resume
//! predicates' equality comparisons match the stored TIMESTAMP exactly;
//! an empty key marks a NULL sort column (the NULLS LAST block).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::indexer::IndexerError;

/// The process-wide cursor-signing key. Random per process unless [`set_key`] ran first
/// (`ESCUREL_CURSOR_KEY`: every replica of one deployment must share it, or a cursor issued by one
/// answers `invalid_cursor` on another).
static KEY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// Fix the signing key from configuration. First call wins; a later one (or one after a cursor was
/// already signed) is ignored, so a running process never changes its mind.
///
/// A key shorter than [`MIN_KEY_BYTES`] is refused: a short secret makes the tag forgeable by guessing.
pub fn set_key(key: &str) -> Result<(), String> {
    check_key(key)?;
    let _ = KEY.set(key.as_bytes().to_vec());
    Ok(())
}

/// The refusal [`set_key`] gives a key that is too short to sign with.
pub fn check_key(key: &str) -> Result<(), String> {
    if key.len() < MIN_KEY_BYTES {
        return Err(format!(
            "ESCUREL_CURSOR_KEY is {} bytes; it must be at least {MIN_KEY_BYTES} (try `openssl rand -hex 32`)",
            key.len()
        ));
    }
    Ok(())
}

/// The shortest signing key accepted from configuration.
pub const MIN_KEY_BYTES: usize = 32;

fn key() -> &'static [u8] {
    KEY.get_or_init(|| {
        let mut k = ulid::Ulid::new().to_bytes().to_vec();
        k.extend_from_slice(&ulid::Ulid::new().to_bytes());
        k
    })
}

fn tag(scope: &str, raw: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac =
        <Hmac<sha2::Sha256> as Mac>::new_from_slice(key()).expect("hmac takes any key length");
    // Length-prefixed, so no scope/payload pair can be rewritten into another.
    mac.update(&(scope.len() as u64).to_be_bytes());
    mac.update(scope.as_bytes());
    mac.update(raw);
    URL_SAFE_NO_PAD.encode(&mac.finalize().into_bytes()[..16])
}

/// `<base64url(raw)>.<tag>`: the opaque, signed token a client passes back as `cursor`, valid only
/// for the same `scope`.
#[must_use]
pub fn seal(scope: &str, raw: &[u8]) -> String {
    format!("{}.{}", URL_SAFE_NO_PAD.encode(raw), tag(scope, raw))
}

/// The bytes [`seal`] signed for this `scope`; `None` for a token this process did not issue for it
/// (malformed, edited, or issued for another tenant, skill or list).
#[must_use]
pub fn unseal(scope: &str, token: &str) -> Option<Vec<u8>> {
    use subtle_eq::ct_eq;
    let (body, sig) = token.rsplit_once('.')?;
    let raw = URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
    ct_eq(sig.as_bytes(), tag(scope, &raw).as_bytes()).then_some(raw)
}

/// Constant-time comparison (the tag is not a secret, but a forger should not learn a prefix by
/// timing).
mod subtle_eq {
    pub(super) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
    }
}

pub(crate) fn encode(scope: &str, sort_key: Option<&str>, row_id: &str) -> String {
    let raw = format!("{}|{}", sort_key.unwrap_or(""), row_id);
    seal(scope, raw.as_bytes())
}

/// → `(sort_key, row_id)`; a malformed cursor is the caller's error.
pub(crate) fn decode(scope: &str, raw: &str) -> Result<(Option<String>, String), IndexerError> {
    let bytes = unseal(scope, raw)
        .ok_or_else(|| IndexerError::InvalidCursor("not a cursor this server issued".to_owned()))?;
    let s = std::str::from_utf8(&bytes)
        .map_err(|e| IndexerError::InvalidCursor(format!("utf-8: {e}")))?;
    let (key, id) = s
        .split_once('|')
        .ok_or_else(|| IndexerError::InvalidCursor("missing separator".to_owned()))?;
    Ok(((!key.is_empty()).then(|| key.to_owned()), id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag covers the scope: a token issued for one tenant/skill/list does not open on another.
    #[test]
    fn a_cursor_opens_only_in_the_scope_it_was_issued_for() {
        let token = seal("instances:acme:note", b"|markdown/instances/note/a.md");
        assert_eq!(
            unseal("instances:acme:note", &token).as_deref(),
            Some(&b"|markdown/instances/note/a.md"[..])
        );
        assert_eq!(unseal("instances:acme:memo", &token), None, "another skill");
        assert_eq!(
            unseal("instances:globex:note", &token),
            None,
            "another tenant"
        );
        assert_eq!(unseal("events:acme", &token), None, "another list");
    }

    /// The scope is length-prefixed in the MAC, so moving a byte between scope and payload is not a
    /// valid forgery.
    #[test]
    fn the_scope_payload_boundary_is_not_ambiguous() {
        let token = seal("ab", b"c");
        assert_eq!(unseal("a", &token), None);
        assert_eq!(unseal("abc", &token), None);
    }

    #[test]
    fn a_signing_key_under_32_bytes_is_refused() {
        assert!(check_key("short").is_err());
        assert!(check_key(&"k".repeat(31)).is_err());
        assert!(check_key(&"k".repeat(32)).is_ok());
        assert!(
            check_key("short")
                .unwrap_err()
                .contains("ESCUREL_CURSOR_KEY"),
            "the refusal names the variable"
        );
    }
}
