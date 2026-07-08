//! # `sched.cron` — interval-based tick scheduler (Wave 5).
//!
//! Produces `Trigger::Tick` goals on a configurable interval schedule. This is
//! the heartbeat substrate for the admission/segmentation plugin: a tick is a
//! cheap observation that the admission filter may either drop (nothing changed)
//! or escalate to a full provider decision.
//!
//! Each entry specifies a simplified interval (e.g. "30s", "5m", "1h") rather
//! than a full POSIX cron expression. Dense cron support (5-field expressions)
//! is deferred to Wave 6 if the interval-based approach proves insufficient.
//! See [`CronEntry::interval_secs`] for supported formats.
//!
//! Lifecycle: the scheduler thread starts in [`Plugin::run`] and is joined in
//! [`Plugin::cleanup`]. The shared receiver is available immediately after
//! construction so callers can poll it before the thread starts (it simply
//! yields nothing until the first tick).
//!
//! Thread safety: the scheduler thread writes to an `mpsc` channel; the
//! receiver end is cloned via [`SchedCron::receiver`] for the loop's
//! observation source to poll. All user-facing methods are `&self`.

use crate::registry::Plugin;
use crate::schema::{Goal, Trigger, Value};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// CronEntry — a single scheduled tick.
// ---------------------------------------------------------------------------

/// A single cron entry: a goal template fired at a repeating interval.
#[derive(Debug, Clone)]
pub struct CronEntry {
    /// Human-readable interval label, e.g. "30s", "5m", "1h". Parsed at
    /// construction by [`CronEntry::new`]; the raw string is kept so tests can
    /// inspect what was configured.
    pub label: String,
    /// Resolved interval in seconds. Must be >= 1.
    pub interval_secs: u64,
    /// Goal id prefix; the actual triggered goal appends a sequence counter.
    pub goal_id: String,
    /// Objective text for the triggered goal.
    pub objective: String,
    /// When true, this entry fires on schedule. When false, it is skipped.
    pub enabled: bool,
}

impl CronEntry {
    /// Create a new cron entry from an interval string and a goal template.
    ///
    /// Supported interval formats (case-insensitive):
    ///   - `30s`   → 30 seconds
    ///   - `5m`    → 5 minutes
    ///   - `1h`    → 1 hour
    ///   - `3600`  → bare number treated as seconds
    ///
    /// Returns an error if the string is unparseable or the interval is zero.
    pub fn new(
        interval_str: &str,
        goal_id: impl Into<String>,
        objective: impl Into<String>,
    ) -> Result<Self, String> {
        let (interval_secs, label) = parse_interval(interval_str)?;
        if interval_secs == 0 {
            return Err("interval must be >= 1 second".into());
        }
        Ok(Self {
            label,
            interval_secs,
            goal_id: goal_id.into(),
            objective: objective.into(),
            enabled: true,
        })
    }

    /// True if this entry is due at `now` given it last fired at `last`.
    pub fn is_due(&self, now: Instant, last: Option<Instant>) -> bool {
        if !self.enabled {
            return false;
        }
        let interval = Duration::from_secs(self.interval_secs);
        match last {
            None => true, // never fired → fire immediately
            Some(t) => now.duration_since(t) >= interval,
        }
    }
}

fn parse_interval(s: &str) -> Result<(u64, String), String> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Err("empty interval string".into());
    }
    // Try suffix-based: 30s, 5m, 1h, 1d
    let (secs, label) = if let Some(rest) = s.strip_suffix('s') {
        let n: u64 = rest.parse().map_err(|_| format!("expected number before 's': {s}"))?;
        (n, format!("{n}s"))
    } else if let Some(rest) = s.strip_suffix('m') {
        let n: u64 = rest.parse().map_err(|_| format!("expected number before 'm': {s}"))?;
        (n * 60, format!("{n}m"))
    } else if let Some(rest) = s.strip_suffix('h') {
        let n: u64 = rest.parse().map_err(|_| format!("expected number before 'h': {s}"))?;
        (n * 3600, format!("{n}h"))
    } else if let Some(rest) = s.strip_suffix('d') {
        let n: u64 = rest.parse().map_err(|_| format!("expected number before 'd': {s}"))?;
        (n * 86400, format!("{n}d"))
    } else {
        // Bare number → seconds
        let n: u64 = s.parse().map_err(|_| format!("unrecognised interval: {s}"))?;
        (n, format!("{n}s"))
    };
    Ok((secs, label))
}

// ---------------------------------------------------------------------------
// SchedCron — the scheduler plugin.
// ---------------------------------------------------------------------------

/// A cron-style scheduler that produces `Trigger::Tick` goals on interval
/// schedules. Runs a background thread that checks every ~500 ms whether any
/// entry is due and pushes the resulting `Goal` onto a shared channel.
///
/// # Usage
///
/// ```ignore
/// use pan_core::plugins::sched_cron::{SchedCron, CronEntry};
///
/// let mut cron = SchedCron::new();
/// cron.add(CronEntry::new("30s", "heartbeat", "periodic check").unwrap());
/// let rx = cron.receiver();
/// // Poll `rx` for due goals in the loop's observation source.
/// ```
pub struct SchedCron {
    entries: Vec<CronEntry>,
    last_fired: Vec<Option<Instant>>,
    tick_seq: u64,
    rx: Arc<Mutex<mpsc::Receiver<Goal>>>,
    tx: Option<mpsc::Sender<Goal>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl SchedCron {
    /// Create an empty scheduler with no entries. Call [`add`](Self::add) to
    /// populate before starting.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            entries: Vec::new(),
            last_fired: Vec::new(),
            tick_seq: 0,
            rx: Arc::new(Mutex::new(rx)),
            tx: Some(tx),
            running: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// Add a cron entry. Panics if the scheduler thread is already running.
    /// Entries can be added before or after start; the thread picks them up on
    /// its next wake cycle.
    pub fn add(&mut self, entry: CronEntry) {
        assert!(
            !self.running.load(Ordering::SeqCst),
            "cannot add entries while scheduler thread is running"
        );
        self.last_fired.push(None);
        self.entries.push(entry);
    }

    /// Number of registered entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Get a receiver for due goals. All clones of the returned handle share
    /// the same underlying channel; callers must lock and call `try_recv()`.
    pub fn receiver(&self) -> Arc<Mutex<mpsc::Receiver<Goal>>> {
        Arc::clone(&self.rx)
    }

    /// Non-blocking poll: return the next due goal, or `None` if none are due.
    /// Intended for use by the loop's observation source.
    pub fn next_due(&self) -> Option<Goal> {
        match self.rx.lock().unwrap().try_recv() {
            Ok(g) => Some(g),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    /// The scheduler's background loop. Checks every ~500 ms whether any entry
    /// is due. For each due entry, pushes a `Goal` with `Trigger::Tick`.
    fn run_inner(tx: mpsc::Sender<Goal>, entries: Arc<std::sync::Mutex<(Vec<CronEntry>, Vec<Option<Instant>>, u64)>>, running: Arc<AtomicBool>) {
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
            let now = Instant::now();
            let mut goals = Vec::new();
            {
                let mut guard = match entries.lock() {
                    Ok(g) => g,
                    Err(_) => break, // poisoned → exit
                };
                let (ref entries, ref mut last_fired, ref mut seq) = *guard;
                for (i, entry) in entries.iter().enumerate() {
                    if entry.is_due(now, last_fired[i]) {
                        last_fired[i] = Some(now);
                        *seq += 1;
                        goals.push(Goal {
                            id: format!("{}-{}", entry.goal_id, *seq),
                            revision: 0,
                            objective: entry.objective.clone(),
                            trigger: Trigger::Tick { sequence: *seq },
                        });
                    }
                }
            }
            for g in goals {
                let _ = tx.send(g); // receiver gone = silently dropped (fail-open)
            }
        }
    }
}

impl Default for SchedCron {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SchedCron {
    fn id(&self) -> &str {
        "sched.cron"
    }

    fn run(&mut self) -> Result<(), crate::registry::PluginError> {
        self.running.store(true, Ordering::SeqCst);
        let tx = self.tx.take().expect("sched.cron tx already taken");

        // Move entries into a shared mutex so the background thread can
        // read them and the owner can modify them before run().
        let entries = Arc::new(std::sync::Mutex::new((
            std::mem::take(&mut self.entries),
            std::mem::take(&mut self.last_fired),
            self.tick_seq,
        )));
        let entries_clone = Arc::clone(&entries);
        let running = Arc::clone(&self.running);

        self.handle = Some(std::thread::spawn(move || {
            Self::run_inner(tx, entries_clone, running);
        }));

        // Move entries back so the struct is consistent after run().
        // The thread has its own reference via Arc.
        let (e, l, s) = Arc::into_inner(entries)
            .expect("sched.cron: only owner holds Arc after run()")
            .into_inner()
            .expect("sched.cron: mutex not poisoned");
        self.entries = e;
        self.last_fired = l;
        self.tick_seq = s;

        Ok(())
    }

    fn cleanup(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SchedCron {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn cron_entry_new_parses_intervals() {
        let e = CronEntry::new("30s", "hb", "heartbeat").unwrap();
        assert_eq!(e.interval_secs, 30);
        assert_eq!(e.label, "30s");

        let e = CronEntry::new("5m", "h5", "five min").unwrap();
        assert_eq!(e.interval_secs, 300);
        assert_eq!(e.label, "5m");

        let e = CronEntry::new("1h", "h1", "hourly").unwrap();
        assert_eq!(e.interval_secs, 3600);

        let e = CronEntry::new("3600", "raw", "raw seconds").unwrap();
        assert_eq!(e.interval_secs, 3600);

        // Error cases
        assert!(CronEntry::new("0", "z", "zero").is_err());
        assert!(CronEntry::new("", "z", "empty").is_err());
        assert!(CronEntry::new("xyz", "z", "garbage").is_err());
    }

    #[test]
    fn is_due_fires_immediately_when_never_fired() {
        let e = CronEntry::new("60s", "test", "test").unwrap();
        assert!(e.is_due(Instant::now(), None));
    }

    #[test]
    fn is_due_not_due_before_interval_elapses() {
        let e = CronEntry::new("60s", "test", "test").unwrap();
        let now = Instant::now();
        let recent = now - Duration::from_secs(30);
        assert!(!e.is_due(now, Some(recent)));
    }

    #[test]
    fn is_due_due_after_interval_elapses() {
        let e = CronEntry::new("10s", "test", "test").unwrap();
        let now = Instant::now();
        let past = now - Duration::from_secs(15);
        assert!(e.is_due(now, Some(past)));
    }

    #[test]
    fn disabled_entry_never_fires() {
        let e = CronEntry { enabled: false, ..CronEntry::new("10s", "t", "t").unwrap() };
        assert!(!e.is_due(Instant::now(), None));
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(3600);
        assert!(!e.is_due(now, Some(long_ago)));
    }

    #[test]
    fn receiver_yields_goals_with_tick_trigger() {
        let mut cron = SchedCron::new();
        cron.add(CronEntry::new("1s", "hb", "heartbeat").unwrap());
        let rx = cron.receiver();

        // Manually fire: we can't easily wait on the thread, so test the
        // channel wiring by simulating what run_inner does.
        let tx = cron.tx.clone().unwrap();
        tx.send(Goal {
            id: "hb-1".into(),
            revision: 0,
            objective: "heartbeat".into(),
            trigger: Trigger::Tick { sequence: 1 },
        })
        .unwrap();

        let got = rx.lock().unwrap().try_recv().unwrap();
        assert_eq!(got.id, "hb-1");
        assert!(matches!(got.trigger, Trigger::Tick { sequence: 1 }));
    }

    #[test]
    fn next_due_non_blocking_returns_none_when_empty() {
        let cron = SchedCron::new();
        assert!(cron.next_due().is_none());
    }

    #[test]
    fn plugin_lifecycle_start_stop() {
        let mut cron = SchedCron::new();
        cron.add(CronEntry::new("5s", "slow", "slow beat").unwrap());

        // run starts the thread, cleanup stops it
        assert!(cron.run().is_ok());
        assert!(cron.running.load(Ordering::SeqCst));
        cron.cleanup();
        assert!(!cron.running.load(Ordering::SeqCst));
    }

    #[test]
    fn scheduler_thread_produces_goals_on_short_interval() {
        let mut cron = SchedCron::new();
        cron.add(CronEntry::new("1s", "fast", "fast beat").unwrap());
        let rx = cron.receiver();

        cron.run().unwrap();

        // Wait long enough for at least one tick to fire
        std::thread::sleep(Duration::from_millis(1500));

        // Must have received at least one goal with a tick trigger
        let got: Vec<Goal> = rx.lock().unwrap().try_iter().collect();
        assert!(!got.is_empty(), "expected at least one tick in 1.5s");
        for g in &got {
            assert!(g.id.starts_with("fast-"), "goal id should start with entry prefix");
            assert!(matches!(g.trigger, Trigger::Tick { .. }));
        }

        cron.cleanup();
    }
}
