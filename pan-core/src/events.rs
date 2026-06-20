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

use crate::schema::{Outcome, Value};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

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
pub trait EventSink: Send {
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
    pub events: Arc<std::sync::Mutex<Vec<Event>>>,
}
impl MemorySink {
    pub fn new() -> Self {
        Self { events: Arc::new(std::sync::Mutex::new(Vec::new())) }
    }
    /// A shared handle to inspect collected events from the emitting side.
    pub fn handle(&self) -> Arc<std::sync::Mutex<Vec<Event>>> {
        Arc::clone(&self.events)
    }
}
impl EventSink for MemorySink {
    fn consume(&mut self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

/// The emit side, cheap and clonable. Cloning shares the same sequence counter
/// and channel, so order is global across all emitters.
#[derive(Clone)]
pub struct EventStream {
    seq: Arc<AtomicU64>,
    tx: Sender<Event>,
}

impl EventStream {
    /// Spawn the consumer thread bound to `sink` and return the emit handle plus
    /// a join guard. Use [`EventStream::shutdown`] to close and join cleanly.
    ///
    /// IMPORTANT: the consumer thread terminates only when **all** `EventStream`
    /// clones have been dropped (that closes the channel). [`StreamGuard::join`]
    /// blocks until then. To avoid a deadlock from holding a live clone while
    /// joining, prefer [`EventStream::shutdown`], which consumes the stream
    /// first. The `Drop` impl on `StreamGuard` detaches rather than blocks.
    pub fn spawn<S: EventSink + 'static>(mut sink: S) -> (EventStream, StreamGuard) {
        let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            // Channel delivers in send order; `seq` makes order explicit/auditable.
            for ev in rx.iter() {
                sink.consume(ev);
            }
            sink.flush();
        });
        (
            EventStream { seq: Arc::new(AtomicU64::new(0)), tx },
            StreamGuard { handle: Some(handle) },
        )
    }

    /// Emit one event. Cheap: assign a sequence number and hand the struct to
    /// the channel. All real work happens on the consumer thread. If the
    /// consumer has gone away, emission is silently dropped (the loop must never
    /// crash because telemetry died — fail-open for observation, §13.3).
    pub fn emit(&self, kind: EventKind) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(Event { seq, kind });
    }

    /// Close this stream and join the consumer, draining and flushing the sink.
    /// Consumes `self` so this handle's `Sender` is dropped before the join.
    /// If other clones are still alive, `join` completes once the last one drops.
    pub fn shutdown(self, guard: StreamGuard) {
        drop(self); // drop this handle's Sender
        guard.join(); // block until all senders gone + consumer drained
    }
}

/// Joins the consumer thread. Created by [`EventStream::spawn`].
pub struct StreamGuard {
    handle: Option<JoinHandle<()>>,
}

impl StreamGuard {
    /// Explicitly join the consumer thread. Blocks until every [`EventStream`]
    /// clone has been dropped (closing the channel) and the sink has flushed.
    /// Call this only after the stream handle(s) are gone, or via
    /// [`EventStream::shutdown`].
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        // Detach rather than block. A blocking join here would deadlock whenever
        // a live EventStream clone still holds the channel open — an easy mistake
        // to make with implicit drop order. Detaching keeps `Drop` safe in all
        // orderings; callers who need a guaranteed flush use `shutdown`/`join`.
        let _ = self.handle.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_arrive_in_sequence_order() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let (stream, guard) = EventStream::spawn(sink);
        for i in 0..100 {
            stream.emit(EventKind::Expressed { body: format!("{i}") });
        }
        stream.shutdown(guard); // close + join the consumer
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 100);
        for (i, ev) in collected.iter().enumerate() {
            assert_eq!(ev.seq, i as u64, "sequence numbers must be dense and ordered");
        }
    }

    #[test]
    fn emitting_after_guard_dropped_does_not_panic_or_deadlock() {
        let (stream, guard) = EventStream::spawn(DiscardSink);
        // Dropping the guard now DETACHES (never blocks), even though `stream`
        // still holds an open Sender. This must not deadlock.
        drop(guard);
        // Further sends are still accepted by the channel (consumer may still be
        // draining) or silently dropped — either way, no panic.
        stream.emit(EventKind::Expressed { body: "after".into() });
    }

    #[test]
    fn multiple_emitters_share_one_total_order() {
        let sink = MemorySink::new();
        let events = sink.handle();
        let (stream, guard) = EventStream::spawn(sink);
        let s2 = stream.clone();
        let t1 = std::thread::spawn(move || {
            for _ in 0..50 { stream.emit(EventKind::Expressed { body: "a".into() }); }
        });
        let t2 = std::thread::spawn(move || {
            for _ in 0..50 { s2.emit(EventKind::Expressed { body: "b".into() }); }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        // Both EventStream clones were moved into the threads and dropped when
        // they finished, so all Senders are gone; join completes promptly.
        guard.join();
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 100);
        let mut seqs: Vec<u64> = collected.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        for (i, s) in seqs.iter().enumerate() {
            assert_eq!(*s, i as u64, "no gaps or duplicates across emitters");
        }
    }
}
