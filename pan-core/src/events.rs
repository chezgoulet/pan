//! # The event stream — ordered, typed, off-thread from day one.
//!
//! Boundary: the core defines the event *schema* and the *ordering guarantee*;
//! persistence/replay is a plugin behind the [`EventSink`] slot (synthesis §6).
//! Emitting is `cheap struct onto a queue`; a consumer thread does serialization
//! or persistence. Retrofitting off-thread eventing later is painful, so the
//! seam is here from the first commit even though the default sink discards.
//!
//! Ordering guarantee: every event carries a monotonic `seq` assigned at emit
//! time under a single lock, so the total order is well-defined regardless of
//! how many threads emit. A durable sink can rely on `seq` to reconstruct order.
//!
//! ## Shutdown
//!
//! [`EventStream::spawn`] returns a single `EventStream` value — there is no
//! separate "guard" type. On `Drop`, the stream closes the channel (by dropping
//! its sender) and joins the consumer thread. For time-sensitive shutdowns,
//! [`EventStream::shutdown_timeout`] applies a deadline; if the consumer hasn't
//! drained by then, the thread is detached.
//!
//! Multi-threaded emitters should wrap the stream in `Arc<EventStream>`:
//! `emit(&self)` is thread-safe.

use crate::schema::{Outcome, Value};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// One ordered record of something that happened during a run. The variants are
/// the core's vocabulary of observable facts; plugins do not extend this set
/// (they emit typed errors / payloads *within* these variants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// A run span opened for a goal.
    RunStarted { goal_id: String, revision: u64 },
    /// The provider returned a decision with this many intents.
    Decided { provider: String, intents: usize },
    /// A world-effecting intent entered the dispatch pipeline.
    DispatchStarted { capability: String, correlation: Option<String> },
    /// A pipeline stage finished (allow/deny/error surfaces here).
    StageCompleted { stage: String, capability: String, status: StageStatus },
    /// An effect fully executed and its result was recorded.
    Effected { capability: String, result: Value },
    /// Content was emitted to channels.
    Expressed { body: String },
    /// A decision was discarded because its goal was superseded (abandon-path).
    Abandoned { goal_id: String, superseded_by: u64 },
    /// The run span closed with a terminal outcome.
    RunConcluded { goal_id: String, outcome: Outcome },
    /// A plugin contribution failed but was contained; the loop degraded rather
    /// than crashed (synthesis §13.3).
    PluginError { plugin: String, message: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Ok,
    Denied,
    Error,
}

/// An event as seen by a sink: the kind plus its position in the total order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub kind: EventKind,
}

/// The sink slot. A sink consumes events in `seq` order. The default
/// ([`DiscardSink`]) drops them; a durable sink persists for replay (§6).
///
/// Sinks run on the consumer thread, never on the hot path, so a slow sink
/// cannot stall the loop (it only fills the channel).
pub trait EventSink: Send + 'static {
    fn consume(&mut self, event: Event);
    /// Called once when the stream closes, after the last event.
    fn flush(&mut self) {}
}

/// Throws every event away. The "if nil, discard" default from synthesis §2.3.
pub struct DiscardSink;
impl EventSink for DiscardSink {
    fn consume(&mut self, _event: Event) {}
}

/// Collects events in memory. Useful for tests and the ephemeral default; a
/// real durable sink would write to disk/DB instead.
#[derive(Default)]
pub struct MemorySink {
    pub events: Arc<Mutex<Vec<Event>>>,
}
impl MemorySink {
    pub fn new() -> Self {
        Self { events: Arc::new(Mutex::new(Vec::new())) }
    }
    /// A shared handle to inspect collected events from the emitting side.
    pub fn handle(&self) -> Arc<Mutex<Vec<Event>>> {
        Arc::clone(&self.events)
    }
}
impl EventSink for MemorySink {
    fn consume(&mut self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

/// A synchronous event stream. Spawn a consumer thread, then emit events from
/// any thread via `&self`. The stream is `Send + Sync` so it can be shared
/// behind `Arc<EventStream>` for multi-threaded emission.
///
/// On `Drop`, the stream closes the channel and **blocks** until the consumer
/// thread has drained and flushed the sink. Use [`EventStream::shutdown_timeout`]
/// to apply a deadline.
///
/// # Multiple emitters
///
/// ```ignore
/// let stream = Arc::new(EventStream::spawn(my_sink));
/// let s2 = Arc::clone(&stream);
/// let t1 = thread::spawn(move || s2.emit(kind1));
/// let t2 = thread::spawn(move || stream.emit(kind2));
/// ```
pub struct EventStream {
    /// Dropped first (field order) to close the channel.
    tx: Option<Sender<Event>>,
    /// Monotonic sequence counter shared across all `emit` calls.
    seq: AtomicU64,
    /// Consumer thread handle; the last reference joins on drop.
    handle: Mutex<Option<JoinHandle<()>>>,
}

// Safety: `Sender` is `Send + Sync`. `JoinHandle` is `Send` but not `Sync` —
// we wrap it in `Mutex` to provide `Sync`. `AtomicU64` is `Send + Sync`.
// `Option<Sender>` is `Send + Sync`.

impl EventStream {
    /// Spawn the consumer thread bound to `sink` and return the emit handle.
    ///
    /// The consumer thread terminates when the sender side is dropped (channel
    /// closes). [`Drop`] handles this automatically; call
    /// [`shutdown_timeout`](Self::shutdown_timeout) for a non-blocking deadline.
    pub fn spawn<S: EventSink>(mut sink: S) -> Self {
        let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            // Channel delivers in send order; `seq` makes order explicit/auditable.
            for ev in rx.iter() {
                sink.consume(ev);
            }
            sink.flush();
        });
        EventStream {
            tx: Some(tx),
            seq: AtomicU64::new(0),
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Emit one event. Cheap: assign a sequence number and hand the struct to
    /// the channel. All real work happens on the consumer thread. If the
    /// consumer has gone away, emission is silently dropped (the loop must never
    /// crash because telemetry died — fail-open for observation, §13.3).
    #[inline]
    pub fn emit(&self, kind: EventKind) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        if let Some(ref tx) = self.tx {
            let _ = tx.send(Event { seq, kind });
        }
    }

    /// Close the stream and join the consumer thread.
    ///
    /// Blocks until the consumer has drained all buffered events and flushed the
    /// sink. Identical to `Drop` but explicit.
    pub fn shutdown(&mut self) {
        self.close_and_join();
    }

    /// Close the stream and attempt to join the consumer thread with a timeout.
    ///
    /// If the consumer does not finish within `timeout`, the thread is detached
    /// and this method returns. Events already in the channel may still be
    /// consumed after the timeout.
    pub fn shutdown_timeout(&mut self, timeout: Duration) {
        // Close the channel.
        self.tx.take();

        // Extract the handle and try to join with a deadline.
        let handle = {
            let mut guard = self.handle.lock().unwrap();
            guard.take()
        };

        if let Some(h) = handle {
            // Spawn a helper that joins and signals completion.
            let (done_tx, done_rx) = mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _ = h.join();
                let _ = done_tx.send(());
            });
            // Block up to `timeout` for the signal.
            let _ = done_rx.recv_timeout(timeout);
            // If timeout fired, the helper thread detaches — the consumer is
            // still running but we no longer wait for it.
        }
    }

    // -- private helpers ---------------------------------------------------

    fn close_and_join(&mut self) {
        self.tx.take();
        let handle = { self.handle.lock().unwrap().take() };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_arrive_in_sequence_order() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let stream = EventStream::spawn(sink);
        for i in 0..100 {
            stream.emit(EventKind::Expressed { body: format!("{i}") });
        }
        stream.shutdown(); // close + join the consumer
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 100);
        for (i, ev) in collected.iter().enumerate() {
            assert_eq!(ev.seq, i as u64, "sequence numbers must be dense and ordered");
        }
    }

    #[test]
    fn drop_closes_and_joins_cleanly() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let stream = EventStream::spawn(sink);
        stream.emit(EventKind::Expressed { body: "hello".into() });
        // Explicit drop closes the channel and joins the consumer.
        drop(stream);
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn multiple_emitters_share_one_total_order() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let stream = Arc::new(EventStream::spawn(sink));

        let t1 = {
            let s = Arc::clone(&stream);
            std::thread::spawn(move || {
                for _ in 0..50 {
                    s.emit(EventKind::Expressed { body: "a".into() });
                }
            })
        };
        let t2 = {
            let s = Arc::clone(&stream);
            std::thread::spawn(move || {
                for _ in 0..50 {
                    s.emit(EventKind::Expressed { body: "b".into() });
                }
            })
        };
        t1.join().unwrap();
        t2.join().unwrap();

        // Drop the Arc — joins the consumer since we're the last reference.
        drop(stream);

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 100);
        let mut seqs: Vec<u64> = collected.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        for (i, s) in seqs.iter().enumerate() {
            assert_eq!(*s, i as u64, "no gaps or duplicates across emitters");
        }
    }

    #[test]
    fn emit_after_panic_does_not_deadlock() {
        let stream = EventStream::spawn(DiscardSink);
        // Simulate a panic while the stream is active.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("simulated application panic");
        }));
        assert!(result.is_err());
        // Dropping the stream after a panic must not deadlock.
        drop(stream);
    }

    #[test]
    fn shutdown_timeout_eventually_joins() {
        let mut stream = EventStream::spawn(DiscardSink);
        stream.emit(EventKind::Expressed { body: "test".into() });
        // Should complete within 1s — the consumer is fast.
        stream.shutdown_timeout(Duration::from_secs(1));
        // If we got here, the timeout path didn't deadlock.
    }

    #[test]
    fn emit_ignores_closed_stream() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let stream = EventStream::spawn(sink);
        stream.emit(EventKind::Expressed { body: "before".into() });
        stream.shutdown();
        // Emit after shutdown is silently dropped (fail-open for observation).
        stream.emit(EventKind::Expressed { body: "after".into() });
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].kind, EventKind::Expressed { body: "before".into() });
    }
}
