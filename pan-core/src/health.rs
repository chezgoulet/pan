//! # Health/observability endpoint.
//!
//! A `/health` HTTP endpoint on a localhost-only admin port. Returns JSON with
//! plugin status, loop status, uptime, and dead-letter count.
//!
//! The health server is optional — it only binds if a non-zero port is
//! configured. Default: localhost:9090.
//!
//! # Integration with plugind
//!
//! The plugin manager pushes health data to [`HealthState::plugin_health`] at
//! regular intervals or on state change. The health server reads the current
//! state and serves it as JSON.

use crate::plugind::PluginHealth;
use serde::Serialize;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ---------------------------------------------------------------------------

/// The system status summary returned by `/health`.
#[derive(Debug, Clone, Serialize)]
pub struct HealthStatus {
    /// Server uptime in seconds.
    pub uptime_secs: u64,
    /// Whether the loop is currently running.
    pub loop_running: bool,
    /// Per-plugin health details.
    pub plugins: Vec<PluginHealth>,
    /// Number of PluginError events that were never recovered (dead letters).
    pub dead_letter_count: u64,
}

/// Mutable health state, shared via `Arc<Mutex<...>>` between the health
/// server thread and the rest of the system (plugind, loop, etc.).
#[derive(Default)]
pub struct HealthState {
    /// When the health server started.
    pub started: Option<Instant>,
    /// Whether the loop is currently running.
    pub loop_running: bool,
    /// Plugin health from plugind.
    pub plugin_health: Vec<PluginHealth>,
    /// Dead-letter counter — incremented on each PluginError event.
    pub dead_letter_count: Arc<AtomicU64>,
}

impl HealthState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a snapshot of the current state for serving via HTTP.
    pub fn snapshot(&self) -> HealthStatus {
        let uptime = self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        HealthStatus {
            uptime_secs: uptime,
            loop_running: self.loop_running,
            plugins: self.plugin_health.clone(),
            dead_letter_count: self.dead_letter_count.load(Ordering::Relaxed),
        }
    }

    /// Record the server start time.
    pub fn set_started(&mut self) {
        self.started = Some(Instant::now());
    }
}

// ---------------------------------------------------------------------------
// Health server
// ---------------------------------------------------------------------------

/// A tiny HTTP server serving `/health` on a localhost port.
///
/// Only one endpoint: `GET /health` → JSON `HealthStatus`.
/// All other paths return 404.
pub struct HealthServer {
    listener: Option<TcpListener>,
    state: Arc<Mutex<HealthState>>,
}

impl HealthServer {
    /// Bind to a localhost port. Pass `0` to disable the server.
    pub fn bind(port: u16) -> Result<Self, std::io::Error> {
        let listener = if port > 0 {
            let addr: SocketAddr = ([127, 0, 0, 1], port).into();
            Some(TcpListener::bind(addr)?)
        } else {
            None
        };
        Ok(HealthServer {
            listener,
            state: Arc::new(Mutex::new(HealthState::new())),
        })
    }

    /// Get a handle to the shared health state for other modules to update.
    pub fn state(&self) -> Arc<Mutex<HealthState>> {
        Arc::clone(&self.state)
    }

    /// Run the health server loop. Blocks until the listener is closed or
    /// errors. Spawn on a dedicated thread:
    ///
    /// ```ignore
    /// let server = HealthServer::bind(9090)?;
    /// let handle = thread::spawn(move || server.run());
    /// ```
    pub fn run(self) {
        let listener = match self.listener {
            Some(l) => l,
            None => return,
        };
        {
            let mut s = self.state.lock().expect("health state lock poisoned");
            s.set_started();
        }
        for conn in listener.incoming() {
            if let Ok(mut stream) = conn {
                let _ = handle_connection(&mut stream, &self.state);
            }
        }
    }
}

fn handle_connection(
    stream: &mut std::net::TcpStream,
    state: &Arc<Mutex<HealthState>>,
) -> Result<(), std::io::Error> {
    use std::io::{Read, Write};

    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let (status_line, content_type, body) = if request.starts_with("GET /health ") {
        let health = state.lock().expect("health state lock poisoned").snapshot();
        let json = serde_json::to_string_pretty(&health)
            .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
        ("HTTP/1.1 200 OK\r\n", "application/json", json)
    } else {
        (
            "HTTP/1.1 404 Not Found\r\n",
            "text/plain",
            "not found".to_string(),
        )
    };

    let response = format!(
        "{status_line}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn health_server_returns_200() {
        let port = free_port();
        let server = HealthServer::bind(port).unwrap();
        let state = server.state();

        // Set up some state.
        {
            let mut s = state.lock().unwrap();
            s.set_started();
            s.loop_running = true;
            s.plugin_health.push(PluginHealth::alive("plugin.test"));
        }

        let handle = thread::spawn(move || server.run());
        thread::sleep(Duration::from_millis(50));

        // Request /health.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        stream.flush().unwrap();

        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();

        assert!(buf.contains("200 OK"), "expected 200, got: {buf}");
        assert!(
            buf.contains("uptime_secs"),
            "expected JSON with uptime_secs"
        );
        assert!(
            buf.contains("plugin.test"),
            "expected plugin health in response"
        );

        drop(stream);
        // Server thread will error on next accept due to socket close.
    }

    #[test]
    fn health_server_returns_404_for_unknown_path() {
        let port = free_port();
        let server = HealthServer::bind(port).unwrap();
        let handle = thread::spawn(move || server.run());
        thread::sleep(Duration::from_millis(50));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        stream.flush().unwrap();

        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        assert!(buf.contains("404 Not Found"));
        drop(stream);
    }

    #[test]
    fn dead_letter_counter_tracks_errors() {
        let counter = Arc::new(AtomicU64::new(0));
        counter.fetch_add(3, Ordering::SeqCst);
        let mut state = HealthState {
            dead_letter_count: counter,
            ..Default::default()
        };
        state.set_started();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.dead_letter_count, 3);
    }

    #[test]
    fn plugin_health_builders() {
        let alive = PluginHealth::alive("p1");
        assert!(alive.alive);
        assert!(alive.error.is_none());

        let degraded = PluginHealth::degraded("p2", "out of memory");
        assert!(!degraded.alive);
        assert_eq!(degraded.error.unwrap(), "out of memory");
    }

    #[test]
    fn bind_port_zero_disables_server() {
        let server = HealthServer::bind(0).unwrap();
        assert!(server.listener.is_none());
    }
}
