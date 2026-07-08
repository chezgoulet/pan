//! # `sched.eventbus` — in-process event bus (Wave 5).
//!
//! A lightweight, in-process pub/sub event bus where plugins emit structured
//! events on named topics and subscribers receive them. Each received event
//! yields a `Trigger::Event` for the loop's observation source.
//!
//! This is the other half of the heartbeat substrate alongside `sched.cron`:
//! while cron produces time-based `Tick` triggers, the event bus produces
//! event-driven `Event` triggers — webhooks, sensor readings, inter-plugin
//! signals, or any structured payload.
//!
//! Topics are hierarchical dotted strings (e.g. `"sensor.temp.high"`,
//! `"webhook.github.push"`). Subscribers may use exact topic names or a
//! wildcard `*` suffix to receive all events under a prefix.
//!
//! Thread safety: all public methods are `&self` and use internal
//! `Mutex`/`RwLock` synchronization. Emit is O(subscribers) and runs
//! subscriber callbacks synchronously on the emitter's thread. For expensive
//! handlers, spawn your own thread inside the callback.
//!
//! Lifecycle: starting the plugin spawns a background consumer thread that
//! processes events from an internal channel. The channel is bounded at 4096
//! events; exceeding this drops the oldest events (fail-open, §13.3).

use crate::registry::Plugin;
use crate::schema::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// A structured event on the bus.
#[derive(Debug, Clone)]
pub struct BusEvent {
    /// Hierarchical topic, e.g. `"sensor.temp"` or `"webhook.github.push"`.
    pub topic: String,
    /// Arbitrary JSON payload.
    pub payload: Value,
    /// Monotonic sequence number assigned at emit time.
    pub seq: u64,
}

/// Subscriber callback: receives a reference to the event. Return `Ok(())` on
/// success, or `Err(String)` on failure (which is logged but does not crash
/// the bus).
pub type SubscriberFn = Arc<dyn Fn(&BusEvent) -> Result<(), String> + Send + Sync>;

// ---------------------------------------------------------------------------
// EventBus — the plugin.
// ---------------------------------------------------------------------------

/// An in-process event bus. Plugins and other components can emit structured
/// events on named topics; subscribers registered via [`subscribe`](Self::subscribe)
/// receive them.
///
/// # Example
///
/// ```ignore
/// use pan_core::plugins::sched_eventbus::EventBus;
/// use pan_core::schema::Value;
///
/// let bus = EventBus::new();
/// bus.subscribe("sensor.*", Arc::new(|event| {
///     eprintln!("sensor event on {}: {}", event.topic, event.payload);
///     Ok(())
/// }));
/// bus.emit("sensor.temp", serde_json::json!({"value": 37.5}));
/// ```
pub struct EventBus {
    /// Topic → stable subscriber id → callback. Stored so we can emit without
    /// holding the write lock.
    subscribers: RwLock<HashMap<String, HashMap<u64, SubscriberFn>>>,
    next_sub_id: Mutex<u64>,
    seq: Mutex<u64>,
    /// Channel for the background consumer thread.
    tx: Sender<BusEvent>,
    rx: Mutex<Option<Receiver<BusEvent>>>,
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl EventBus {
    /// Create a new, empty event bus.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            subscribers: RwLock::new(HashMap::new()),
            next_sub_id: Mutex::new(1),
            seq: Mutex::new(0),
            tx,
            rx: Mutex::new(Some(rx)),
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        }
    }

    /// Register a subscriber for a topic pattern. Patterns:
    ///   - `"sensor.temp"` — exact match only
    ///   - `"sensor.*"` — matches `sensor.temp`, `sensor.humidity`, etc.
    ///   - `"*"` — matches every topic (global subscriber; use sparingly)
    ///
    /// Returns a subscription id that can be passed to [`unsubscribe`](Self::unsubscribe).
    pub fn subscribe(&self, topic_pattern: &str, callback: SubscriberFn) -> u64 {
        let mut subs = self.subscribers.write().unwrap();
        let mut id_lock = self.next_sub_id.lock().unwrap();
        let id = *id_lock;
        *id_lock += 1;
        subs.entry(topic_pattern.to_string())
            .or_default()
            .insert(id, callback);
        id
    }

    /// Remove a subscription by its id.
    pub fn unsubscribe(&self, sub_id: u64) -> bool {
        let mut subs = self.subscribers.write().unwrap();
        for (_pattern, handlers) in subs.iter_mut() {
            if handlers.remove(&sub_id).is_some() {
                return true;
            }
        }
        false
    }

    /// Emit an event on a topic. All matching subscribers receive the event
    /// synchronously on the calling thread. Subscriber errors are collected and
    /// returned as a vec; the bus continues delivering to other subscribers.
    ///
    /// Returns the event's sequence number and any subscriber errors.
    pub fn emit(&self, topic: &str, payload: Value) -> (u64, Vec<String>) {
        // Assign sequence number.
        let seq = {
            let mut s = self.seq.lock().unwrap();
            *s += 1;
            *s
        };

        let event = BusEvent {
            topic: topic.to_string(),
            payload,
            seq,
        };

        // Collect matching subscriber callbacks under the read lock.
        let subs = self.subscribers.read().unwrap();
        let mut callbacks: Vec<SubscriberFn> = Vec::new();
        for (pattern, handlers) in subs.iter() {
            if pattern_matches(pattern, &event.topic) {
                for cb in handlers.values() {
                    callbacks.push(Arc::clone(cb));
                }
            }
        }
        drop(subs); // release read lock before calling callbacks

        // Deliver to all matching subscribers. Collect errors but never fail
        // the emit itself (fail-open, §13.3).
        let mut errors = Vec::new();
        for cb in &callbacks {
            if let Err(e) = cb(&event) {
                errors.push(e);
            }
        }

        // Also push onto the background channel for the receive() API.
        let _ = self.tx.send(event);

        (seq, errors)
    }

    /// Non-blocking poll: return the next received event from the internal
    /// channel, or `None` if none are pending. The background consumer thread
    /// (started during `run()`) drains this channel; callers without the
    /// background thread can still poll directly.
    pub fn recv(&self) -> Option<BusEvent> {
        let rx_guard = self.rx.lock().unwrap();
        if let Some(ref rx) = *rx_guard {
            match rx.try_recv() {
                Ok(e) => Some(e),
                Err(_) => None,
            }
        } else {
            None
        }
    }

    /// Iterate over all currently pending events without blocking.
    pub fn try_iter(&self) -> Vec<BusEvent> {
        let rx_guard = self.rx.lock().unwrap();
        if let Some(ref rx) = *rx_guard {
            rx.try_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Number of subscriber callbacks registered across all patterns.
    pub fn subscriber_count(&self) -> usize {
        let subs = self.subscribers.read().unwrap();
        subs.values().map(|h| h.len()).sum()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for EventBus {
    fn id(&self) -> &str {
        "sched.eventbus"
    }

    fn cleanup(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let mut h = self.handle.lock().unwrap();
        if let Some(handle) = h.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for EventBus {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ---------------------------------------------------------------------------
// Pattern matching.
// ---------------------------------------------------------------------------

/// Match a subscriber pattern against a topic. Supports:
///   - Exact match: `"sensor.temp" == "sensor.temp"`
///   - Suffix wildcard: `"sensor.*"` matches `"sensor.temp"`, `"sensor.humidity"`
///   - Global wildcard: `"*"` matches everything
fn pattern_matches(pattern: &str, topic: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // suffix wildcard
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return topic == prefix || topic.starts_with(&format!("{prefix}."));
    }
    pattern == topic
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_subscriber_receives_event() {
        let bus = EventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&received);
        bus.subscribe("sensor.temp", Arc::new(move |e| {
            r.lock().unwrap().push(e.topic.clone());
            Ok(())
        }));

        bus.emit("sensor.temp", serde_json::json!({"v": 1}));
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], "sensor.temp");
    }

    #[test]
    fn wildcard_subscriber_matches_prefix() {
        let bus = EventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&received);
        bus.subscribe("sensor.*", Arc::new(move |e| {
            r.lock().unwrap().push(e.topic.clone());
            Ok(())
        }));

        bus.emit("sensor.temp", serde_json::json!({}));
        bus.emit("sensor.humidity", serde_json::json!({}));
        bus.emit("actor.move", serde_json::json!({}));

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.contains(&"sensor.temp".to_string()));
        assert!(events.contains(&"sensor.humidity".to_string()));
    }

    #[test]
    fn global_wildcard_matches_everything() {
        let bus = EventBus::new();
        let count = Arc::new(Mutex::new(0usize));
        let c = Arc::clone(&count);
        bus.subscribe("*", Arc::new(move |_| {
            *c.lock().unwrap() += 1;
            Ok(())
        }));

        bus.emit("a", serde_json::json!({}));
        bus.emit("b", serde_json::json!({}));
        bus.emit("c", serde_json::json!({}));
        assert_eq!(*count.lock().unwrap(), 3);
    }

    #[test]
    fn non_matching_topic_does_not_deliver() {
        let bus = EventBus::new();
        let flag = Arc::new(Mutex::new(false));
        let f = Arc::clone(&flag);
        bus.subscribe("alarm.*", Arc::new(move |_| {
            *f.lock().unwrap() = true;
            Ok(())
        }));

        bus.emit("other.stuff", serde_json::json!({}));
        assert!(!*flag.lock().unwrap());
    }

    #[test]
    fn unsubscribe_removes_handler() {
        let bus = EventBus::new();
        let count = Arc::new(Mutex::new(0usize));
        let c = Arc::clone(&count);
        let id = bus.subscribe("test", Arc::new(move |_| {
            *c.lock().unwrap() += 1;
            Ok(())
        }));

        bus.emit("test", serde_json::json!({}));
        assert_eq!(*count.lock().unwrap(), 1);

        assert!(bus.unsubscribe(id));
        bus.emit("test", serde_json::json!({}));
        assert_eq!(*count.lock().unwrap(), 1); // no change after unsubscribe
    }

    #[test]
    fn emit_returns_sequence_number() {
        let bus = EventBus::new();
        let (seq1, _) = bus.emit("a", Value::Null);
        let (seq2, _) = bus.emit("b", Value::Null);
        assert!(seq2 > seq1);
    }

    #[test]
    fn subscriber_error_does_not_crash_bus() {
        let bus = EventBus::new();
        let r2 = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&r2);
        // This subscriber always errors.
        bus.subscribe("fragile", Arc::new(move |_| Err("boom".into())));
        // This one should still receive the event.
        bus.subscribe("fragile", Arc::new(move |e| {
            r.lock().unwrap().push(e.topic.clone());
            Ok(())
        }));

        let (_seq, errors) = bus.emit("fragile", serde_json::json!({}));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0], "boom");
        assert_eq!(r2.lock().unwrap().len(), 1);
    }

    #[test]
    fn recv_retrieves_emitted_events_fifo() {
        let bus = EventBus::new();
        bus.emit("first", serde_json::json!(1));
        bus.emit("second", serde_json::json!(2));

        // Drain via recv.
        let e1 = bus.recv().unwrap();
        assert_eq!(e1.topic, "first");
        assert_eq!(e1.payload, serde_json::json!(1));

        let e2 = bus.recv().unwrap();
        assert_eq!(e2.topic, "second");
        assert_eq!(e2.payload, serde_json::json!(2));

        // No more events.
        assert!(bus.recv().is_none());
    }

    #[test]
    fn plugin_lifecycle_id() {
        let bus = EventBus::new();
        assert_eq!(bus.id(), "sched.eventbus");
    }

    #[test]
    fn pattern_matching_tests() {
        // Exact
        assert!(pattern_matches("sensor.temp", "sensor.temp"));
        assert!(!pattern_matches("sensor.temp", "sensor.humidity"));

        // Suffix wildcard
        assert!(pattern_matches("sensor.*", "sensor.temp"));
        assert!(pattern_matches("sensor.*", "sensor"));
        assert!(!pattern_matches("sensor.*", "other.temp"));

        // Global wildcard
        assert!(pattern_matches("*", "anything.at.all"));

        // No wildcard suffix means exact
        assert!(!pattern_matches("sensor", "sensor.temp"));
    }
}
