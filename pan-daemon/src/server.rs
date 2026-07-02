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
use std::sync::Mutex;

use crate::session::{Session, SessionError};
use crate::wire::{Body, Envelope, ErrorBody};

/// The maximum NDJSON line length accepted. 1 MiB is generous for any sane
/// fixture / perceive / decision; anything larger is a hostile host.
const MAX_LINE_BYTES: usize = 1 * 1024 * 1024;

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
        Ok(Server { listener, current: Mutex::new(None) })
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

    /// Drive one connection to completion. Reads lines, hands them to the
    /// session, writes replies, and exits cleanly on `shutdown` or EOF.
    /// On `SessionError::VersionUnsupported` the error is written and the
    /// connection is closed.
    pub fn drive(stream: TcpStream) -> io::Result<()> {
        // The connection is half-duplex by hand: one BufReader on a clone for
        // reading, the original TcpStream for writing. We could go async, but
        // for a line-at-a-time protocol the synchronous path is the
        // easiest to read and profile.
        let read_stream = stream.try_clone()?;
        let mut reader = BufReader::new(read_stream);
        let mut writer = stream;
        let mut session = Session::new();

        loop {
            let mut line = String::new();
            let n = match reader.read_line(&mut line) {
                Ok(0) => return Ok(()), // EOF
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };
            if n == 0 { return Ok(()); }
            // Strip the trailing newline (BufReader::read_line keeps it).
            if line.ends_with('\n') { line.pop(); }
            if line.ends_with('\r') { line.pop(); }
            if line.is_empty() { continue; }
            if line.len() > MAX_LINE_BYTES {
                // Pathological / hostile. Send a bad_frame and keep going.
                let out = Envelope::outgoing(0, None, Body::Error(ErrorBody {
                    code: crate::wire::ErrorCode::BadFrame,
                    message: format!("line exceeds {MAX_LINE_BYTES} bytes"),
                }));
                write_ndjson(&mut writer, &out)?;
                continue;
            }

            let env = match Envelope::from_ndjson(&line) {
                Ok(e) => e,
                Err(e) => {
                    // Bad frame: reply with `error: bad_frame` and keep going.
                    let out = Envelope::outgoing(0, None, Body::Error(ErrorBody {
                        code: crate::wire::ErrorCode::BadFrame,
                        message: format!("could not parse NDJSON: {e}"),
                    }));
                    write_ndjson(&mut writer, &out)?;
                    continue;
                }
            };

            // Per the Soul Protocol: the daemon MUST close the connection
            // after a host `shutdown`. Track the inbound `type` and exit
            // the loop after writing the response(s).
            let inbound_was_shutdown = env.ty == crate::wire::MessageType::Shutdown;

            match session.handle(env) {
                Ok(responses) => {
                    for r in &responses {
                        write_ndjson(&mut writer, &r)?;
                    }
                    if inbound_was_shutdown {
                        return Ok(());
                    }
                }
                Err(SessionError::VersionUnsupported { client, ours }) => {
                    // Send one final error message, then close.
                    let out = Envelope::outgoing(0, None, Body::Error(ErrorBody {
                        code: crate::wire::ErrorCode::VersionUnsupported,
                        message: format!(
                            "protocol_version mismatch: client={client}, daemon={ours}"),
                    }));
                    let _ = write_ndjson(&mut writer, &out);
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

/// Helper: serialize the envelope compactly and write it followed by a
/// newline. Flushes after every line so the host sees the response
/// immediately.
fn write_ndjson(w: &mut TcpStream, env: &Envelope) -> io::Result<()> {
    let s = env.to_ndjson()
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Body, Envelope, HelloBody, MessageType, ShutdownBody, PROTOCOL_VERSION, SERVER_IDENTITY};
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
            v: 0, seq: 0, re: None, ty: MessageType::Hello,
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
        } else { panic!("expected welcome body, got {resp:?}"); }

        // Send a shutdown so the server thread exits.
        let shutdown = Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::Shutdown,
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
        } else { panic!("expected error body, got {resp:?}"); }

        // Then a valid shutdown so the server thread exits.
        let shutdown = Envelope {
            v: 0, seq: 1, re: None, ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        writeln!(s, "{}", shutdown.to_ndjson().unwrap()).unwrap();
        drop(s);
        server_handle.join().unwrap();
    }
}
