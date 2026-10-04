//! A log writer that can never stall the process.
//!
//! `tracing_subscriber`'s `with_writer(io::stdout)` writes every event synchronously, from whichever
//! thread logged it, while holding the stdout lock. When the consumer of stdout stops reading (a stuck
//! log shipper, a `docker logs` that backs up, a harness that read the connection line and nothing
//! else) the 64 KiB pipe fills after ~90 requests, the write blocks, the thread that holds the lock
//! stays parked, and every other thread that logs queues behind it: the whole gateway answers
//! nothing, for ever, and the orchestrator sees a live process with a healthy-looking `/healthz`
//! that is not listening to anyone.
//!
//! Here events go through a bounded queue to ONE writer thread, the only place a write may block.
//! A full queue DROPS the event and counts it. Losing a log line is the right trade against losing
//! the service, and the loss is not silent: [`dropped_log_lines`] feeds a metric, and once the reader
//! is back the writer says how many lines it lost.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// Events queued for the writer thread before new ones are dropped (~8 MiB at ~1 KiB per line).
pub const DEFAULT_CAPACITY: usize = 8192;

/// A queued line is at most this long. The queue is bounded in LINES, so without a per-line cap a few
/// thousand multi-megabyte lines (a logged request body, a stack of errors) would be gigabytes of
/// memory; an oversized line is replaced by a small valid JSON notice that says how big it was.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// How often, at most, the writer reports lines it had to drop.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// Process-wide count of log lines dropped because the log consumer was not reading.
static DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Log lines dropped so far because the log consumer was not reading (all writers in this process).
pub fn dropped_log_lines() -> u64 {
    DROPPED_TOTAL.load(Ordering::Relaxed)
}

enum Msg {
    Line(Vec<u8>),
    Flush(SyncSender<()>),
}

/// Builds the JSON line that says `n` lines were dropped (the writer thread owns the format).
pub type DropNotice = Box<dyn Fn(u64) -> Vec<u8> + Send + 'static>;

struct Inner {
    tx: SyncSender<Msg>,
    dropped: AtomicU64,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// A `MakeWriter` whose writes never block the caller. Cheap to clone; all clones share one queue.
#[derive(Clone)]
pub struct NonBlockingWriter {
    inner: Arc<Inner>,
}

impl NonBlockingWriter {
    /// Write to `sink` from a dedicated thread; keep at most `capacity` events queued.
    pub fn new<W>(sink: W, capacity: usize, notice: DropNotice) -> Self
    where
        W: Write + Send + 'static,
    {
        let (tx, rx) = sync_channel(capacity.max(1));
        let inner = Arc::new(Inner {
            tx,
            dropped: AtomicU64::new(0),
            worker: Mutex::new(None),
        });
        let worker_inner = Arc::clone(&inner);
        let handle = std::thread::Builder::new()
            .name("escurel-log-writer".to_owned())
            .spawn(move || run(rx, sink, &worker_inner, notice))
            .expect("spawn the log writer thread");
        *inner.worker.lock().expect("log writer handle") = Some(handle);
        Self { inner }
    }

    /// Lines THIS writer dropped.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Wait (up to `timeout`) until everything queued so far has been written. Best effort: a reader
    /// that is not reading makes this time out, which is the point of not blocking on it.
    pub fn flush_within(&self, timeout: Duration) {
        let (ack_tx, ack_rx) = sync_channel(1);
        if self.inner.tx.try_send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(timeout);
        }
    }
}

fn run<W: Write>(rx: Receiver<Msg>, mut sink: W, inner: &Inner, notice: DropNotice) {
    let mut reported = 0u64;
    let mut last_report: Option<Instant> = None;
    // Report what was dropped, at most once per REPORT_EVERY.
    let mut report = |sink: &mut W, reported: &mut u64, last_report: &mut Option<Instant>| {
        let dropped = inner.dropped.load(Ordering::Relaxed);
        if dropped > *reported && last_report.is_none_or(|t| t.elapsed() >= REPORT_EVERY) {
            let _ = sink
                .write_all(&notice(dropped - *reported))
                .and_then(|()| sink.flush());
            *reported = dropped;
            *last_report = Some(Instant::now());
        }
    };
    loop {
        // A timeout (not just a line) is a reason to wake: a loss is reported even when nothing else
        // is logged afterwards (a quiet process whose log reader has just come back).
        match rx.recv_timeout(REPORT_EVERY) {
            Ok(Msg::Line(line)) => {
                // A failed write (a closed pipe) discards the line: nobody is reading, and the loop
                // must keep draining so the queue never backs up into the callers.
                let _ = sink.write_all(&line).and_then(|()| sink.flush());
                report(&mut sink, &mut reported, &mut last_report);
            }
            Ok(Msg::Flush(ack)) => {
                let _ = sink.flush();
                let _ = ack.send(());
            }
            Err(RecvTimeoutError::Timeout) => report(&mut sink, &mut reported, &mut last_report),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// One event's worth of bytes. `tracing_subscriber` hands the formatted line to the writer, which
/// is queued when it is dropped (so a line written in several chunks still travels whole).
pub struct LineSink {
    inner: Arc<Inner>,
    buf: Vec<u8>,
}

impl Write for LineSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LineSink {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let mut line = std::mem::take(&mut self.buf);
        if line.len() > MAX_LINE_BYTES {
            line = truncated_notice(&line);
        }
        match self.inner.tx.try_send(Msg::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// The line that replaces one over [`MAX_LINE_BYTES`]: valid JSON with the original size and a prefix.
fn truncated_notice(line: &[u8]) -> Vec<u8> {
    let head_len = 1024.min(line.len());
    let mut head = String::from_utf8_lossy(&line[..head_len]).into_owned();
    // Keep the prefix a plain string: it may be cut mid-JSON and mid-codepoint.
    head.retain(|c| c != '\u{fffd}');
    let mut out = serde_json::to_vec(&serde_json::json!({
        "level": "warn",
        "msg": "log line truncated: over the per-line size cap",
        "original_bytes": line.len(),
        "cap_bytes": MAX_LINE_BYTES,
        "prefix": head,
    }))
    .unwrap_or_default();
    out.push(b'\n');
    out
}

impl<'a> MakeWriter<'a> for NonBlockingWriter {
    type Writer = LineSink;

    fn make_writer(&'a self) -> LineSink {
        LineSink {
            inner: Arc::clone(&self.inner),
            buf: Vec::with_capacity(256),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// A sink that blocks every write until released: a log consumer that has stopped reading.
    struct StuckSink {
        gate: std::sync::mpsc::Receiver<()>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for StuckSink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let _ = self.gate.recv(); // blocks until the test opens the gate (or drops it)
            self.written.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn log(w: &NonBlockingWriter, line: &str) {
        let mut sink = w.make_writer();
        sink.write_all(line.as_bytes()).unwrap();
    }

    #[test]
    fn a_stuck_consumer_never_blocks_the_logging_thread_and_the_loss_is_counted() {
        let (open, gate) = channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let w = NonBlockingWriter::new(
            StuckSink {
                gate,
                written: Arc::clone(&written),
            },
            4,
            Box::new(|n| format!("DROPPED {n}\n").into_bytes()),
        );

        // The consumer is stuck from the first byte. A thousand lines must all return at once.
        let started = Instant::now();
        for i in 0..1000 {
            log(&w, &format!("line {i}\n"));
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "logging blocked behind a consumer that is not reading"
        );
        // One line is in the writer's hands (blocked), `capacity` are queued, the rest were dropped.
        assert!(w.dropped() >= 990, "dropped {}", w.dropped());
        assert!(dropped_log_lines() >= w.dropped());

        // The consumer comes back: what was kept is written, and the loss is announced.
        for _ in 0..64 {
            let _ = open.send(());
        }
        std::thread::sleep(REPORT_EVERY + Duration::from_millis(200));
        log(&w, "after\n");
        w.flush_within(Duration::from_secs(5));
        let out = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(out.contains("line 0\n"), "the first line is kept: {out:?}");
        assert!(out.contains("DROPPED "), "the loss is reported: {out:?}");
        assert!(out.contains("after\n"), "later lines flow again: {out:?}");
    }

    /// A sink that blocks every write while `blocked` is set.
    struct Tripwire {
        blocked: Arc<std::sync::atomic::AtomicBool>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Tripwire {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            while self.blocked.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.written.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_loss_is_reported_on_a_timer_even_if_no_further_line_is_logged() {
        let blocked = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let written = Arc::new(Mutex::new(Vec::new()));
        let w = NonBlockingWriter::new(
            Tripwire {
                blocked: Arc::clone(&blocked),
                written: Arc::clone(&written),
            },
            1,
            Box::new(|n| format!("DROPPED {n}\n").into_bytes()),
        );
        let settle = || std::thread::sleep(Duration::from_millis(150));
        // Round 1: 10 lines lost, then the reader comes back; the loss is reported right away.
        for i in 0..12 {
            log(&w, &format!("a{i}\n"));
            settle();
        }
        blocked.store(false, Ordering::SeqCst);
        settle();
        assert!(String::from_utf8_lossy(&written.lock().unwrap()).contains("DROPPED "));
        // Round 2, within the report interval: 5 more lines lost, the writer then goes quiet. The
        // loss must still be announced (by the timer), though no later line arrives to trigger it.
        blocked.store(true, Ordering::SeqCst);
        for i in 0..8 {
            log(&w, &format!("b{i}\n"));
            std::thread::sleep(Duration::from_millis(20));
        }
        blocked.store(false, Ordering::SeqCst);
        std::thread::sleep(REPORT_EVERY * 3);
        let out = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let notices = out.matches("DROPPED ").count();
        assert!(notices >= 2, "the second loss was never reported: {out:?}");
    }

    #[test]
    fn an_oversized_line_is_replaced_by_a_small_valid_json_notice() {
        let written = Arc::new(Mutex::new(Vec::new()));
        struct Collect(Arc<Mutex<Vec<u8>>>);
        impl Write for Collect {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let w = NonBlockingWriter::new(Collect(Arc::clone(&written)), 16, Box::new(|_| Vec::new()));
        let big = format!("{{\"msg\":\"{}\"}}\n", "x".repeat(10 * 1024 * 1024));
        log(&w, &big);
        log(&w, "{\"msg\":\"small\"}\n");
        w.flush_within(Duration::from_secs(5));
        let out = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(
            out.len() < 2 * MAX_LINE_BYTES,
            "the queue is bounded in bytes too: {}",
            out.len()
        );
        let first = out.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(first).expect("a valid JSON line");
        assert_eq!(v["original_bytes"], big.len());
        assert!(out.contains("small"), "later lines are untouched");
    }

    #[test]
    fn a_line_written_in_chunks_travels_whole() {
        let written = Arc::new(Mutex::new(Vec::new()));
        struct Collect(Arc<Mutex<Vec<u8>>>);
        impl Write for Collect {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let w = NonBlockingWriter::new(Collect(Arc::clone(&written)), 16, Box::new(|_| Vec::new()));
        let mut sink = w.make_writer();
        sink.write_all(b"{\"a\":").unwrap();
        sink.write_all(b"1}\n").unwrap();
        drop(sink);
        w.flush_within(Duration::from_secs(5));
        assert_eq!(&*written.lock().unwrap(), b"{\"a\":1}\n");
    }
}
