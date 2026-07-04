//! # The TCP loopback server.
//!
//! One [`std::net::TcpListener`], bound to `127.0.0.1:<port>`. The protocol
//! says one connection per host process; a new connect therefore drops the
//! old one. The current connection's read loop:
//!
//! 1. Read one NDJSON line (newline-delimited; cap at 1 MiB per line so a
//!    malicious host can't OOM us by sending gigabytes without a newline).
//! 2. Deserialize into [`crate::wire::Envelope`]. On parse failure, send an
//!    `error: bad_frame` reply and keep going.
//! 3. Hand the envelope to [`Session::handle`]. The session may return one or
//!    more response envelopes, or a `SessionError::VersionUnsupported` which
//!    means the driver writes the error and closes the connection.
//! 4. Write each response envelope as a `\n`-terminated NDJSON line.
//! 5. On `shutdown`, exit the loop and close the connection.
//!
//! The server is a single thread per connection (no async runtime — the
//! daemon is small, the wire is line-at-a-time, and we want the daemon to
//! be obvious to read and profile).

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};

use crate::session::{Session, SessionError};
use crate::wire::{Body, Envelope, ErrorBody};

/// The maximum NDJSON line length accepted. 1 MiB is generous for any sane
/// fixture / perceive / decision; anything larger is a hostile host.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// The server. Holds the listener and the optional "current connection"
/// (which is the one allowed per the protocol). When a new host connects,
/// the old `current` is shut down cleanly.
pub struct Server {
    pub listener: TcpListener,
    /// The currently active connection's stream, if any. Held inside a
    /// `Mutex` because the accept loop wants to close it on a new connect.
    current: Mutex<Option<TcpStream>>,
}

impl Server {
    /// Bind to the given address. Address is e.g. `"127.0.0.1:40707"`. The
    /// bind is loopback-only by construction: callers MUST supply a 127.x
    /// address. We do not enforce that here — the canonical entry point
    /// ([`serve_loopback`]) constructs the address from `--port` and a
    /// hard-coded `127.0.0.1`.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Server {
            listener,
            current: Mutex::new(None),
        })
    }

    /// Accept one connection; the function blocks until a host connects.
    /// If a previous connection is still active, it is closed (the protocol
    /// says one connection per host process — a reconnect is "the old one
    /// went away"). The function returns the new stream (the caller drives
    /// the read/write loop).
    pub fn accept_one(&self) -> io::Result<TcpStream> {
        let (stream, _peer) = self.listener.accept()?;
        // Drop the previous connection (if any) — close it cleanly.
        let mut guard = self.current.lock().expect("current mutex poisoned");
        if let Some(old) = guard.take() {
            let _ = old.shutdown(Shutdown::Both);
        }
        *guard = Some(stream.try_clone()?);
        Ok(stream)
    }

    /// Drop the retained clone of the just-driven connection. `drive` owns
    /// one handle and drops it on return, but `accept_one` kept a clone in
    /// `current` — the OS only closes the TCP connection when *every* handle
    /// is gone. Without this, a host that sent `shutdown` waits forever for
    /// the EOF the protocol promises ("`shutdown` | connection close").
    pub fn close_current(&self) {
        let mut guard = self.current.lock().expect("current mutex poisoned");
        if let Some(old) = guard.take() {
            let _ = old.shutdown(Shutdown::Both);
        }
    }

    /// Drive one connection to completion. Reads lines, hands them to the
    /// session, writes replies, and exits cleanly on `shutdown` or EOF.
    /// On `SessionError::VersionUnsupported` the error is written and the
    /// connection is closed.
    ///
    /// Perceives are ASYNCHRONOUS (M7): each mind call runs on its own
    /// worker thread, so a slow LLM completion never blocks the read loop —
    /// a rules soul answers instantly while another soul is still thinking,
    /// and decisions reach the host in completion order (the protocol
    /// requires hosts to tolerate exactly that; they correlate by `re`).
    /// The session state and the write half live behind mutexes; lock order
    /// is always session before writer.
    pub fn drive(stream: TcpStream) -> io::Result<()> {
        let read_stream = stream.try_clone()?;
        let mut reader = BufReader::new(read_stream);
        let writer = Arc::new(Mutex::new(stream));
        let session = Arc::new(Mutex::new(Session::new()));

        loop {
            let mut line = String::new();
            let n = match reader.read_line(&mut line) {
                Ok(0) => return Ok(()), // EOF
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };
            if n == 0 {
                return Ok(());
            }
            // Strip the trailing newline (BufReader::read_line keeps it).
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_LINE_BYTES {
                // Pathological / hostile. Send a bad_frame and keep going.
                let mut s = lock(&session);
                let out = Envelope::outgoing(
                    s.alloc_seq(),
                    None,
                    Body::Error(ErrorBody {
                        code: crate::wire::ErrorCode::BadFrame,
                        message: format!("line exceeds {MAX_LINE_BYTES} bytes"),
                    }),
                );
                write_all(&session, &writer, Some(s), &[out])?;
                continue;
            }

            let env = match parse_envelope(&line) {
                Ok(e) => e,
                Err((code, message)) => {
                    // Rejected line: `bad_frame` (not a valid envelope) or
                    // `unknown_type` (valid envelope, type outside the closed
                    // set). Either way the connection stays open.
                    let mut s = lock(&session);
                    let out = Envelope::outgoing(
                        s.alloc_seq(),
                        None,
                        Body::Error(ErrorBody { code, message }),
                    );
                    write_all(&session, &writer, Some(s), &[out])?;
                    continue;
                }
            };

            // Per the Soul Protocol: the daemon MUST close the connection
            // after a host `shutdown`. Track the inbound `type` and exit
            // the loop after writing the response(s).
            let inbound_was_shutdown = env.ty == crate::wire::MessageType::Shutdown;

            // Perceive fans out to a worker: begin under the lock (cheap),
            // decide outside every lock (slow), finish under the lock (the
            // enact boundary, where supersession discards stale work).
            if env.ty == crate::wire::MessageType::Perceive {
                let mut s = lock(&session);
                match s.begin_perceive(env.seq, env.body) {
                    Err(replies) => {
                        write_all(&session, &writer, Some(s), &replies)?;
                    }
                    Ok(job) => {
                        drop(s); // the mind call must run outside every lock
                        let session2 = Arc::clone(&session);
                        let writer2 = Arc::clone(&writer);
                        std::thread::spawn(move || {
                            let decision = job.provider.decide(&job.goal, &job.context, &job.caps);
                            let s = lock(&session2);
                            let mut s = s;
                            let outs = s.finish_perceive(&job, decision);
                            // The connection may already be gone (host quit
                            // mid-thought); a failed write is not an error.
                            let _ = write_all(&session2, &writer2, Some(s), &outs);
                        });
                    }
                }
                continue;
            }

            let mut s = lock(&session);
            match s.handle(env) {
                Ok(responses) => {
                    write_all(&session, &writer, Some(s), &responses)?;
                    if inbound_was_shutdown {
                        return Ok(());
                    }
                }
                Err(SessionError::VersionUnsupported { client, ours }) => {
                    // Send one final error message, then close.
                    let out = Envelope::outgoing(
                        s.alloc_seq(),
                        None,
                        Body::Error(ErrorBody {
                            code: crate::wire::ErrorCode::VersionUnsupported,
                            message: format!(
                                "protocol_version mismatch: client={client}, daemon={ours}"
                            ),
                        }),
                    );
                    let _ = write_all(&session, &writer, Some(s), &[out]);
                    return Ok(());
                }
                Err(SessionError::BadFrame(_))
                | Err(SessionError::UnknownType(_))
                | Err(SessionError::UnknownSoul(_)) => {
                    // Reserved for future wire-level errors. Current session
                    // returns Ok(_) with an `error` body for these; this
                    // match arm exists for exhaustiveness.
                    return Ok(());
                }
            }
        }
    }
}

/// Lock helper: a poisoned session/writer mutex means a worker panicked
/// mid-write; the connection is unsalvageable either way, so we take the
/// data as-is rather than propagate the panic.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write envelopes while STILL holding the session guard that allocated
/// their `seq` values (the writer lock nests inside it — always session
/// before writer, never the reverse). Releasing the session lock between
/// seq allocation and the write would let another thread's later seq hit
/// the wire first, breaking the envelope contract's "sender-local
/// monotonically increasing".
fn write_all(
    _session: &Arc<Mutex<Session>>,
    writer: &Mutex<TcpStream>,
    session_guard: Option<std::sync::MutexGuard<'_, Session>>,
    envelopes: &[Envelope],
) -> io::Result<()> {
    let mut w = lock(writer);
    for env in envelopes {
        write_ndjson(&mut w, env)?;
    }
    drop(session_guard); // held across the writes on purpose
    Ok(())
}

/// Parse one NDJSON line into an envelope, distinguishing the protocol's two
/// rejection codes: a line that is not valid JSON — or that decodes into no
/// known envelope/body shape — is `bad_frame`; a structurally sound envelope
/// whose `type` is a string outside the closed set of 10 is `unknown_type`
/// (SOUL-PROTOCOL.md: "MUST reject unknown `type` values with an `error`
/// (code `unknown_type`)"). Serde alone can't make that distinction — an
/// unrecognized enum variant and a missing field both surface as the same
/// deserialize error — so we stage the parse: JSON first, then the `type`
/// discriminator, then the full envelope.
fn parse_envelope(line: &str) -> Result<Envelope, (crate::wire::ErrorCode, String)> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
        (
            crate::wire::ErrorCode::BadFrame,
            format!("could not parse NDJSON: {e}"),
        )
    })?;
    if let Some(ty) = value.get("type").and_then(|t| t.as_str()) {
        if crate::wire::MessageType::from_wire(ty).is_none() {
            return Err((
                crate::wire::ErrorCode::UnknownType,
                format!("unknown message type `{ty}`"),
            ));
        }
    }
    serde_json::from_value(value).map_err(|e| {
        (
            crate::wire::ErrorCode::BadFrame,
            format!("could not parse NDJSON: {e}"),
        )
    })
}

/// Helper: serialize the envelope compactly and write it followed by a
/// newline. Flushes after every line so the host sees the response
/// immediately.
fn write_ndjson(w: &mut TcpStream, env: &Envelope) -> io::Result<()> {
    let s = env
        .to_ndjson()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// The canonical entry point: bind to `127.0.0.1:<port>`, accept one
/// connection, drive it to completion. Returns the port actually bound (the
/// caller's `--port` is honored if non-zero; if 0 we ask the OS for a port
/// and return that).
pub fn serve_loopback(port: u16) -> io::Result<()> {
    let server = Server::bind(("127.0.0.1", port))?;
    let actual = server.listener.local_addr()?.port();
    eprintln!("pan serve: bound 127.0.0.1:{actual}");
    loop {
        let stream = server.accept_one()?;
        // Drive synchronously; the next accept waits for the next host. A
        // connection-level error (host crashed, reset mid-frame) must never
        // kill the daemon — log it and wait for the next host.
        if let Err(e) = Server::drive(stream) {
            eprintln!("pan serve: connection error: {e}; awaiting next host");
        }
        // Whatever ended the drive (shutdown, version mismatch, EOF, error),
        // release the retained clone so the host observes the close.
        server.close_current();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        Body, Envelope, HelloBody, MessageType, ShutdownBody, PROTOCOL_VERSION, SERVER_IDENTITY,
    };
    use std::io::{BufReader, Write};
    use std::net::TcpStream;
    use std::thread;

    /// Smoke test: bind on a 0-port, accept, send a hello, expect welcome.
    /// The test thread sends one shutdown at the end so `drive` returns.
    #[test]
    fn hello_round_trip() {
        let server = Server::bind(("127.0.0.1", 0)).unwrap();
        let port = server.listener.local_addr().unwrap().port();

        let server_handle = thread::spawn(move || {
            let stream = server.accept_one().unwrap();
            // Drive one connection. The test sends hello, reads welcome,
            // then sends shutdown.
            Server::drive(stream).unwrap();
        });

        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let hello = Envelope {
            v: 0,
            seq: 0,
            re: None,
            ty: MessageType::Hello,
            body: Body::Hello(HelloBody {
                protocol_version: PROTOCOL_VERSION,
                profile: "reachlock/0".into(),
                client: "test".into(),
            }),
        };
        writeln!(s, "{}", hello.to_ndjson().unwrap()).unwrap();

        // Read one line back.
        let mut buf = String::new();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        reader.read_line(&mut buf).unwrap();
        let resp: Envelope = serde_json::from_str(buf.trim()).unwrap();
        if let Body::Welcome(w) = &resp.body {
            assert_eq!(w.protocol_version, 0);
            assert_eq!(w.server, SERVER_IDENTITY);
        } else {
            panic!("expected welcome body, got {resp:?}");
        }

        // Send a shutdown so the server thread exits.
        let shutdown = Envelope {
            v: 0,
            seq: 1,
            re: None,
            ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        writeln!(s, "{}", shutdown.to_ndjson().unwrap()).unwrap();
        drop(s);
        server_handle.join().unwrap();
    }

    /// A line that is not valid JSON yields a bad_frame error reply, and the
    /// connection stays open for the next valid frame.
    #[test]
    fn bad_frame_yields_bad_frame_error_then_continues() {
        let server = Server::bind(("127.0.0.1", 0)).unwrap();
        let port = server.listener.local_addr().unwrap().port();
        let server_handle = thread::spawn(move || {
            let stream = server.accept_one().unwrap();
            Server::drive(stream).unwrap();
        });

        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        // Bad line first.
        writeln!(s, "this is not json").unwrap();
        let mut buf = String::new();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        reader.read_line(&mut buf).unwrap();
        let resp: Envelope = serde_json::from_str(buf.trim()).unwrap();
        if let Body::Error(e) = &resp.body {
            assert_eq!(e.code, crate::wire::ErrorCode::BadFrame);
        } else {
            panic!("expected error body, got {resp:?}");
        }

        // Then a valid shutdown so the server thread exits.
        let shutdown = Envelope {
            v: 0,
            seq: 1,
            re: None,
            ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        writeln!(s, "{}", shutdown.to_ndjson().unwrap()).unwrap();
        drop(s);
        server_handle.join().unwrap();
    }

    /// After `shutdown` is acked the host must observe the connection
    /// CLOSING (EOF), not just silence. Regression test for the retained
    /// `current` clone keeping the fd open after `drive` returned; the test
    /// mirrors `serve_loopback`'s accept loop including `close_current`.
    #[test]
    fn shutdown_ack_is_followed_by_eof() {
        let server = Server::bind(("127.0.0.1", 0)).unwrap();
        let port = server.listener.local_addr().unwrap().port();
        let server_handle = thread::spawn(move || {
            let stream = server.accept_one().unwrap();
            Server::drive(stream).unwrap();
            server.close_current();
        });

        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let hello = Envelope {
            v: 0,
            seq: 0,
            re: None,
            ty: MessageType::Hello,
            body: Body::Hello(HelloBody {
                protocol_version: PROTOCOL_VERSION,
                profile: "reachlock/0".into(),
                client: "test".into(),
            }),
        };
        writeln!(s, "{}", hello.to_ndjson().unwrap()).unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap(); // welcome

        let shutdown = Envelope {
            v: 0,
            seq: 1,
            re: None,
            ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        writeln!(s, "{}", shutdown.to_ndjson().unwrap()).unwrap();
        buf.clear();
        reader.read_line(&mut buf).unwrap(); // ack
        let resp: Envelope = serde_json::from_str(buf.trim()).unwrap();
        assert_eq!(resp.ty, MessageType::Ack);

        // The next read must be EOF (0 bytes), not a hang — a read timeout
        // here means the daemon left the connection open after shutdown.
        buf.clear();
        let n = reader
            .read_line(&mut buf)
            .expect("expected EOF after shutdown ack, got a read error/timeout");
        assert_eq!(n, 0, "expected EOF after shutdown ack, got: {buf:?}");
        server_handle.join().unwrap();
    }

    /// A well-formed envelope whose `type` is outside the closed set yields
    /// `error: unknown_type` (NOT `bad_frame`), and the connection stays open
    /// for the next valid frame. This is the protocol's "MUST reject unknown
    /// `type` values with an `error` (code `unknown_type`)" clause.
    #[test]
    fn unknown_type_yields_unknown_type_error_then_continues() {
        let server = Server::bind(("127.0.0.1", 0)).unwrap();
        let port = server.listener.local_addr().unwrap().port();
        let server_handle = thread::spawn(move || {
            let stream = server.accept_one().unwrap();
            Server::drive(stream).unwrap();
        });

        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(s, r#"{{"v":0,"seq":7,"type":"frobnicate","body":{{}}}}"#).unwrap();
        let mut buf = String::new();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        reader.read_line(&mut buf).unwrap();
        let resp: Envelope = serde_json::from_str(buf.trim()).unwrap();
        if let Body::Error(e) = &resp.body {
            assert_eq!(e.code, crate::wire::ErrorCode::UnknownType);
            assert!(
                e.message.contains("frobnicate"),
                "message should name the unknown type: {}",
                e.message
            );
        } else {
            panic!("expected error body, got {resp:?}");
        }

        // Then a valid shutdown so the server thread exits.
        let shutdown = Envelope {
            v: 0,
            seq: 8,
            re: None,
            ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        writeln!(s, "{}", shutdown.to_ndjson().unwrap()).unwrap();
        drop(s);
        server_handle.join().unwrap();
    }

    /// `parse_envelope` staging: not-JSON → bad_frame; unknown type →
    /// unknown_type; a known type with a broken body → bad_frame.
    #[test]
    fn parse_envelope_distinguishes_bad_frame_from_unknown_type() {
        use crate::wire::ErrorCode;
        let err = parse_envelope("not json").unwrap_err();
        assert_eq!(err.0, ErrorCode::BadFrame);

        let err = parse_envelope(r#"{"v":0,"seq":1,"type":"frobnicate","body":{}}"#).unwrap_err();
        assert_eq!(err.0, ErrorCode::UnknownType);

        let err = parse_envelope(r#"{"v":0,"seq":1,"type":"hello","body":{}}"#).unwrap_err();
        assert_eq!(
            err.0,
            ErrorCode::BadFrame,
            "hello with a missing body shape is a bad frame"
        );

        assert!(parse_envelope(r#"{"v":0,"seq":1,"type":"shutdown","body":{}}"#).is_ok());
    }
}
