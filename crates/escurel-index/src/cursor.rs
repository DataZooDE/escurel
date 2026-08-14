//! Opaque list-cursor codec shared by the paged list surfaces:
//! base64url(`"<sort-key-or-empty>|<row-id>"`). The sort key is stored
//! at FULL microsecond precision (`%Y-%m-%d %H:%M:%S.%f`) so resume
//! predicates' equality comparisons match the stored TIMESTAMP exactly;
//! an empty key marks a NULL sort column (the NULLS LAST block).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::indexer::IndexerError;

pub(crate) fn encode(sort_key: Option<&str>, row_id: &str) -> String {
    let raw = format!("{}|{}", sort_key.unwrap_or(""), row_id);
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// → `(sort_key, row_id)`; a malformed cursor is the caller's error.
pub(crate) fn decode(raw: &str) -> Result<(Option<String>, String), IndexerError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.as_bytes())
        .map_err(|e| IndexerError::InvalidCursor(format!("base64: {e}")))?;
    let s = std::str::from_utf8(&bytes)
        .map_err(|e| IndexerError::InvalidCursor(format!("utf-8: {e}")))?;
    let (key, id) = s
        .split_once('|')
        .ok_or_else(|| IndexerError::InvalidCursor("missing separator".to_owned()))?;
    Ok(((!key.is_empty()).then(|| key.to_owned()), id.to_owned()))
}
