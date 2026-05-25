//! Error type surfaced by [`crate::Client`]. Maps the underlying
//! tonic transport / status failures into a small, semver-tracked
//! enum so downstream apps don't pin `tonic` directly.
//!
//! See `docs/spec/dx.md` §"Stability and versioning" — additions to
//! this enum are *breaking* (per the spec), so keep the surface
//! intentionally minimal.

use thiserror::Error;
use tonic::Status;
use tonic::transport::Error as TransportError;

/// Failures surfaced by [`crate::Client`] methods.
#[derive(Debug, Error)]
pub enum Error {
    /// The endpoint URL passed to [`crate::Client::connect`] failed
    /// to parse. The inner string is the offending URL.
    #[error("invalid endpoint URL `{0}`")]
    InvalidEndpoint(String),

    /// The bearer token contains characters that are not legal in
    /// an HTTP header value (control bytes, non-ASCII, etc.).
    #[error("invalid bearer token: not a valid HTTP header value")]
    InvalidToken,

    /// The TCP/gRPC connect handshake to the gateway failed —
    /// network unreachable, refused, TLS error, …
    #[error("failed to connect to gateway: {0}")]
    Connect(#[source] TransportError),

    /// The RPC reached the server and returned a non-`Ok` status
    /// (`Unauthenticated`, `ResourceExhausted`, `NotFound`, …).
    /// Inspect `.code()` for routing.
    #[error("rpc failed: {0}")]
    Rpc(#[source] Status),
}

impl From<Status> for Error {
    fn from(s: Status) -> Self {
        Self::Rpc(s)
    }
}
