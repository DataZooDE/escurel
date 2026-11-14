//! The `sha256=<hex>` HMAC-SHA256 body signature every escurel webhook uses:
//! the gateway signs the capture webhook it sends to a runner's `/trigger`,
//! the runner verifies it, and the gateway verifies the schedule webhook it
//! receives on `/hooks/schedule/<skill>` (#827). One implementation, so the
//! two ends cannot drift.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The header carrying the signature of the raw request body.
pub const SIGNATURE_HEADER: &str = "X-Escurel-Webhook-Signature";

/// `sha256=<lowercase hex>` HMAC-SHA256 of `body` under `secret`.
#[must_use]
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body);
    format!("sha256={:x}", mac.finalize().into_bytes())
}

/// Whether `presented` is `body`'s signature under `secret`. `false` for a
/// missing or malformed header. The compare is constant-time
/// (`Mac::verify_slice`).
#[must_use]
pub fn verify(secret: &str, body: &[u8], presented: Option<&str>) -> bool {
    let Some(expected) = presented
        .and_then(|p| p.strip_prefix("sha256="))
        .and_then(decode_hex)
    else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Hex (either case) as bytes; `None` for an odd length or a non-hex digit.
#[must_use]
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_matches_the_published_vector_and_verifies() {
        let body = b"The quick brown fox jumps over the lazy dog";
        let sig = sign("key", body);
        assert_eq!(
            sig,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        assert!(verify("key", body, Some(&sig)));
        assert!(verify(
            "key",
            body,
            Some(&sig.to_uppercase().replace("SHA256=", "sha256="))
        ));
    }

    #[test]
    fn anything_else_does_not_verify() {
        let sig = sign("key", b"body");
        assert!(!verify("other", b"body", Some(&sig)));
        assert!(!verify("key", b"body!", Some(&sig)));
        assert!(!verify("key", b"body", None));
        assert!(!verify(
            "key",
            b"body",
            Some(sig.trim_start_matches("sha256="))
        ));
        assert!(!verify("key", b"body", Some("sha256=zz")));
        assert!(!verify("key", b"body", Some("sha256=abc")));
        assert!(!verify("key", b"body", Some("sha256=é0")));
    }
}
