//! Opaque list-cursor codec shared by the paged list surfaces:
//! `<base64url("<sort-key-or-empty>|<row-id>")>.<base64url(HMAC tag)>`. Every cursor family
//! (instances, events, chat, rows, drafts) goes through [`seal`] / [`unseal`], so a token the server
//! did not issue — made up, or edited — is refused as `invalid_cursor` instead of being decoded. The sort key is stored
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
pub fn set_key(key: &str) {
    let _ = KEY.set(key.as_bytes().to_vec());
}

fn key() -> &'static [u8] {
    KEY.get_or_init(|| {
        let mut k = ulid::Ulid::new().to_bytes().to_vec();
        k.extend_from_slice(&ulid::Ulid::new().to_bytes());
        k
    })
}

fn tag(raw: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac =
        <Hmac<sha2::Sha256> as Mac>::new_from_slice(key()).expect("hmac takes any key length");
    mac.update(raw);
    URL_SAFE_NO_PAD.encode(&mac.finalize().into_bytes()[..16])
}

/// `<base64url(raw)>.<tag>`: the opaque, signed token a client passes back as `cursor`.
#[must_use]
pub fn seal(raw: &[u8]) -> String {
    format!("{}.{}", URL_SAFE_NO_PAD.encode(raw), tag(raw))
}

/// The bytes [`seal`] signed; `None` for a token this process did not issue (malformed, or edited).
#[must_use]
pub fn unseal(token: &str) -> Option<Vec<u8>> {
    use subtle_eq::ct_eq;
    let (body, sig) = token.rsplit_once('.')?;
    let raw = URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
    ct_eq(sig.as_bytes(), tag(&raw).as_bytes()).then_some(raw)
}

/// Constant-time comparison (the tag is not a secret, but a forger should not learn a prefix by
/// timing).
mod subtle_eq {
    pub(super) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
    }
}

pub(crate) fn encode(sort_key: Option<&str>, row_id: &str) -> String {
    let raw = format!("{}|{}", sort_key.unwrap_or(""), row_id);
    seal(raw.as_bytes())
}

/// → `(sort_key, row_id)`; a malformed cursor is the caller's error.
pub(crate) fn decode(raw: &str) -> Result<(Option<String>, String), IndexerError> {
    let bytes = unseal(raw)
        .ok_or_else(|| IndexerError::InvalidCursor("not a cursor this server issued".to_owned()))?;
    let s = std::str::from_utf8(&bytes)
        .map_err(|e| IndexerError::InvalidCursor(format!("utf-8: {e}")))?;
    let (key, id) = s
        .split_once('|')
        .ok_or_else(|| IndexerError::InvalidCursor("missing separator".to_owned()))?;
    Ok(((!key.is_empty()).then(|| key.to_owned()), id.to_owned()))
}
