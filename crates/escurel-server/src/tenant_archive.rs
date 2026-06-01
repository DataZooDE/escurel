//! Transport-neutral tar+gz helpers for tenant export/import.
//!
//! These were originally inline in `grpc.rs`; they were lifted here
//! so the (then gRPC `EscurelAdmin`, now MCP-over-HTTP)
//! `tenant_export` / `tenant_import` tools share one
//! transport-neutral implementation.
//!
//! `tar::Archive::unpack` rejects entries containing `..` segments
//! by default, so the path-traversal surface mirrors the rest of
//! the admin layer (see [`escurel_admin::validate_tenant_id`] for
//! the tenant-id half of the same defence).

use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;

/// Build a gzip-framed tar of `root`'s contents and feed it through
/// `sink` in `chunk` byte slices. Runs synchronously — the caller
/// hosts it on a blocking thread.
///
/// `sink` returns `Err` when the consumer has hung up; we surface
/// that as `io::Error` to abort the tar stream.
pub(crate) fn tar_gz_into_chunks(
    root: &Path,
    chunk: usize,
    mut sink: impl FnMut(Vec<u8>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let writer = ChunkSink {
        chunk,
        buf: Vec::with_capacity(chunk),
        emit: &mut sink,
    };
    let gz = GzEncoder::new(writer, Compression::default());
    let mut tar = tar::Builder::new(gz);
    // Append everything under `root` as the archive root — i.e.
    // entries are stored as their path *relative to* `root`. On
    // import the consumer extracts back into another `root`, so
    // names round-trip without nesting.
    tar.append_dir_all(".", root)?;
    let gz = tar.into_inner()?;
    let mut writer = gz.finish()?;
    writer.flush_remaining()
}

/// Sink used by [`tar_gz_into_chunks`] — accumulates writes into a
/// reusable buffer and forwards full `chunk`-byte slices through
/// `emit`. The trailing fragment is emitted by `flush_remaining`.
pub(crate) struct ChunkSink<'a> {
    chunk: usize,
    buf: Vec<u8>,
    emit: &'a mut dyn FnMut(Vec<u8>) -> std::io::Result<()>,
}

impl ChunkSink<'_> {
    fn flush_remaining(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let out = std::mem::take(&mut self.buf);
            (self.emit)(out)?;
        }
        Ok(())
    }
}

impl std::io::Write for ChunkSink<'_> {
    fn write(&mut self, mut data: &[u8]) -> std::io::Result<usize> {
        let total = data.len();
        while !data.is_empty() {
            let want = self.chunk - self.buf.len();
            let take = data.len().min(want);
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() >= self.chunk {
                let out = std::mem::take(&mut self.buf);
                self.buf.reserve(self.chunk);
                (self.emit)(out)?;
            }
        }
        Ok(total)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Inverse of [`tar_gz_into_chunks`]: decode `bytes` as gzip+tar
/// and extract every entry under `dest`. Runs synchronously — the
/// caller hosts it on a blocking thread.
///
/// `tar::Archive::unpack` rejects entries containing `..` segments
/// by default, so the path-traversal surface mirrors the rest of
/// the admin layer.
pub(crate) fn untar_gz_into(bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dest)
}
