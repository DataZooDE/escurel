//! [`ReloadableEmbedder`] — a hot-swappable [`Embedder`] seam.
//!
//! The server binary builds the real embedder at startup. When the
//! model fails to load (missing safetensors, no network egress on a
//! cold cache, …) the binary must still boot in a *degraded* state:
//! it swaps in a placeholder [`ZeroEmbedder`], marks itself
//! not-ready, and serves `/healthz` so the orchestrator can probe
//! it. The `embedding_reload` admin RPC then retries the real load
//! and, on success, calls [`ReloadableEmbedder::reload`] to swap the
//! live inner embedder *without restarting the process*.
//!
//! The swap is lock-free via `arc_swap::ArcSwap`, so an in-flight
//! `embed` call never blocks a reload and vice versa — the reload
//! just publishes a new pointer that subsequent `embed` calls pick
//! up.
//!
//! ## Dimension invariant
//!
//! A reload must not change the vector dimension: the indexer pins
//! `blocks.dense_vec` to a fixed width at construction time, so a
//! mid-flight dimension change would corrupt the column. The binary
//! is responsible for only handing [`reload`](ReloadableEmbedder::reload)
//! an embedder whose `dim()` matches; this type does not re-validate
//! (it has no schema handle), it only reports the current `dim()`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use async_trait::async_trait;

use crate::{EmbedError, Embedder, ZeroEmbedder};

/// A hot-swappable [`Embedder`].
///
/// Delegates every `embed`/`dim` call to the current inner embedder.
/// [`reload`](Self::reload) atomically publishes a new inner; an
/// `is_loaded` flag tracks whether the live inner is the real model
/// (`true`) or the degraded-start [`ZeroEmbedder`] placeholder
/// (`false`).
pub struct ReloadableEmbedder {
    inner: ArcSwap<DynEmbedder>,
    /// `false` while the live inner is the degraded placeholder.
    /// Surfaced to `/readyz` as `embedder: <is_loaded>`. A plain
    /// atomic suffices — the flag is not pointer-coupled to the
    /// inner (the placeholder is just another `Arc<dyn Embedder>`
    /// and is not otherwise distinguishable at the trait boundary).
    loaded: AtomicBool,
}

/// `ArcSwap` requires the stored pointer's pointee be `Sized`, so we
/// store a sized newtype around the trait object rather than the
/// trait object directly.
struct DynEmbedder(Arc<dyn Embedder>);

impl ReloadableEmbedder {
    /// Build around a successfully-loaded real embedder. Reports
    /// [`is_loaded`](Self::is_loaded) `== true`.
    #[must_use]
    pub fn loaded(inner: Arc<dyn Embedder>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(DynEmbedder(inner)),
            loaded: AtomicBool::new(true),
        }
    }

    /// Build in the degraded state: the live inner is a
    /// [`ZeroEmbedder`] of `dim`, [`is_loaded`](Self::is_loaded) is
    /// `false`. The binary uses this when the real model fails to
    /// load at startup, then retries via [`reload`](Self::reload).
    #[must_use]
    pub fn degraded(dim: usize) -> Self {
        Self {
            inner: ArcSwap::from_pointee(DynEmbedder(Arc::new(ZeroEmbedder::new(dim)))),
            loaded: AtomicBool::new(false),
        }
    }

    /// Atomically swap the live inner embedder to `new` and mark the
    /// embedder loaded. Lock-free: in-flight `embed` calls finish
    /// against the old inner; subsequent calls see `new`.
    pub fn reload(&self, new: Arc<dyn Embedder>) {
        self.inner.store(Arc::new(DynEmbedder(new)));
        self.loaded.store(true, Ordering::Release);
    }

    /// Swap back to a degraded [`ZeroEmbedder`] placeholder of `dim`
    /// and mark not-loaded. Used if a live model needs to be torn
    /// down (e.g. a future health-driven eviction); the binary's
    /// startup path uses [`degraded`](Self::degraded) directly.
    pub fn degrade(&self, dim: usize) {
        self.inner
            .store(Arc::new(DynEmbedder(Arc::new(ZeroEmbedder::new(dim)))));
        self.loaded.store(false, Ordering::Release);
    }

    /// Whether the live inner is the real model (`true`) or the
    /// degraded placeholder (`false`). `/readyz` surfaces this as
    /// the `embedder` component.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }
}

#[async_trait]
impl Embedder for ReloadableEmbedder {
    fn dim(&self) -> usize {
        self.inner.load().0.dim()
    }

    fn model_id(&self) -> String {
        self.inner.load().0.model_id()
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Pin the current inner for the duration of the call so a
        // concurrent `reload` cannot drop it mid-`embed`. Cloning the
        // inner `Arc<dyn Embedder>` out of the snapshot keeps the
        // embedder alive across the `.await` without holding the
        // `ArcSwap` guard.
        let inner = Arc::clone(&self.inner.load().0);
        inner.embed(texts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn degraded_then_reload_flips_is_loaded_and_swaps_inner() {
        // A degraded embedder reports not-loaded and produces zero
        // vectors of the configured dim.
        let r = ReloadableEmbedder::degraded(768);
        assert!(!r.is_loaded());
        assert_eq!(r.dim(), 768);
        let v = r.embed(&["hi"]).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].len(), 768);

        // Reloading a real (here: another stub) embedder flips the
        // flag and routes subsequent calls to the new inner.
        r.reload(Arc::new(ZeroEmbedder::new(768)));
        assert!(r.is_loaded());
        assert_eq!(r.dim(), 768);
    }

    #[tokio::test]
    async fn loaded_constructor_reports_loaded() {
        let r = ReloadableEmbedder::loaded(Arc::new(ZeroEmbedder::new(256)));
        assert!(r.is_loaded());
        assert_eq!(r.dim(), 256);
    }
}
