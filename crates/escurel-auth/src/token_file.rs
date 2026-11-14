//! A bearer a sidecar (or an operator) keeps fresh in a file: the runner's
//! `ESCUREL_RUNNER_TOKEN_FILE` and the chat bridge's
//! `ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE`. One implementation, so a rotated
//! credential is picked up the same way by both: re-read when the file's
//! mtime changes, and on demand ([`TokenFile::reload`]) when the gateway
//! rejected the token in hand. A file that changes to something unusable
//! keeps the last good token, logged.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// The file could not be read, or holds nothing.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{label} {path}: {reason}")]
pub struct TokenFileError {
    /// The env var that named the file, for the operator.
    pub label: &'static str,
    pub path: String,
    pub reason: String,
}

/// A token read from a file, re-read when it changes.
pub struct TokenFile {
    label: &'static str,
    path: PathBuf,
    /// `(token, the file's mtime when read)`.
    cached: Mutex<(String, Option<SystemTime>)>,
}

impl std::fmt::Debug for TokenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenFile")
            .field("label", &self.label)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl TokenFile {
    /// Read `path` now: a process configured with a token file it cannot
    /// read must not start as if it had no credential.
    ///
    /// # Errors
    /// When the file is unreadable or empty.
    pub fn open(label: &'static str, path: impl Into<PathBuf>) -> Result<Self, TokenFileError> {
        let path = path.into();
        let read = read(label, &path)?;
        Ok(Self {
            label,
            path,
            cached: Mutex::new(read),
        })
    }

    /// The token: the cached one, re-read first when the file's mtime moved.
    #[must_use]
    pub fn current(&self) -> String {
        let mtime = mtime(&self.path);
        let mut slot = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if mtime.is_some() && mtime != slot.1 {
            self.replace(&mut slot);
        }
        slot.0.clone()
    }

    /// Re-read the file whatever its mtime says (the gateway rejected the
    /// token in hand: a rotation in the same second, or on a filesystem with
    /// coarse mtimes). `true` when the token is now a different one.
    pub fn reload(&self) -> bool {
        let mut slot = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        let before = slot.0.clone();
        self.replace(&mut slot);
        slot.0 != before
    }

    fn replace(&self, slot: &mut (String, Option<SystemTime>)) {
        match read(self.label, &self.path) {
            Ok(fresh) => {
                if fresh.0 != slot.0 {
                    tracing::info!(path = %self.path.display(),
                        "{} changed; using the new credential", self.label);
                }
                *slot = fresh;
            }
            Err(e) => tracing::warn!(error = %e,
                "{} changed but is unusable; keeping the last token", self.label),
        }
    }
}

fn mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn read(
    label: &'static str,
    path: &std::path::Path,
) -> Result<(String, Option<SystemTime>), TokenFileError> {
    let err = |reason: String| TokenFileError {
        label,
        path: path.display().to_string(),
        reason,
    };
    let mtime = mtime(path);
    let token = std::fs::read_to_string(path)
        .map_err(|e| err(e.to_string()))?
        .trim()
        .to_owned();
    if token.is_empty() {
        return Err(err("the file is empty".to_owned()));
    }
    Ok((token, mtime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_file_is_picked_up_and_an_emptied_one_keeps_the_last_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "one\n").unwrap();
        let file = TokenFile::open("TEST_TOKEN_FILE", &path).unwrap();
        assert_eq!(file.current(), "one");
        std::fs::write(&path, "two").unwrap();
        assert!(file.reload(), "a forced re-read sees the rotation");
        assert_eq!(file.current(), "two");
        assert!(!file.reload(), "unchanged");
        std::fs::write(&path, "  ").unwrap();
        assert!(!file.reload());
        assert_eq!(
            file.current(),
            "two",
            "an emptied file keeps the last token"
        );
        let err = TokenFile::open("TEST_TOKEN_FILE", dir.path().join("missing")).unwrap_err();
        assert!(err.to_string().starts_with("TEST_TOKEN_FILE "), "{err}");
    }
}
