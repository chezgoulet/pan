use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use pan_core::schema::Scope;

use crate::session::{dispatch_decision_async, Session, SessionError};
use crate::wire::{Body, Envelope, ErrorBody};

const MAX_LINE_BYTES: usize = 1024 * 1024;

pub struct Server {
    pub listener: TcpListener,
}

impl Server {
    pub async fn bind<A: tokio::net::ToSocketAddrs>(addr: A) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Server { listener })
    }

    pub async fn accept_one(&self) -> std::io::Result<TcpStream> {
        let (stream, _peer) = self.listener.accept().await?;
        Ok(stream)
    }

    pub async fn drive(stream: TcpStream) -> std::io::Result<()> {
        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        let session = Arc::new(Mutex::new(Session::new()));

        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let n = reader.read_line(&mut line_buf).await?;
            if n == 0 {
                return Ok(());
            }
            if line_buf.ends_with('\n') {
                line_buf.pop();
            }
            if line_buf.ends_with('\r') {
                line_buf.pop();
            }
            if line_buf.is_empty() {
                continue;
            }
            if line_buf.len() > MAX_LINE_BYTES {
                let out = {
                    let mut s = session.lock().unwrap();
                    Envelope::outgoing(
                        s.alloc_seq(),
                        None,
                        Body::Error(ErrorBody {
                            code: crate::wire::ErrorCode::BadFrame,
                            message: format!("line exceeds {MAX_LINE_BYTES} bytes"),
                        }),
                    )
                };
                write_one(&writer, &out).await;
                continue;
            }

            let env = {
                let env = parse_envelope(&line_buf);
                match env {
                    Ok(e) => e,
                    Err((code, message)) => {
                        let out = {
                            let mut s = session.lock().unwrap();
                            Envelope::outgoing(
                                s.alloc_seq(),
                                None,
                                Body::Error(ErrorBody { code, message }),
                            )
                        };
                        write_one(&writer, &out).await;
                        continue;
                    }
                }
            };

            let inbound_was_shutdown = env.ty == crate::wire::MessageType::Shutdown;

            if env.ty == crate::wire::MessageType::Perceive {
                let (err_replies, job) = {
                    let mut s = session.lock().unwrap();
                    match s.begin_perceive(env.seq, env.body) {
                        Err(replies) => (Some(replies), None),
                        Ok(j) => (None, Some(j)),
                    }
                };
                if let Some(replies) = err_replies {
                    for reply in replies {
                        write_one(&writer, &reply).await;
                    }
                    continue;
                }
                if let Some(job) = job {
                    let session2 = Arc::clone(&session);
                    let writer2 = Arc::clone(&writer);
                    tokio::spawn(async move {
                        let decision = job
                            .provider
                            .decide(&job.goal, &job.context, &job.caps)
                            .await;
                        let scope = Scope::new(format!("soul.{}", job.soul_id));
                        let outcome =
                            dispatch_decision_async(&decision, &job.registry, &scope).await;
                        let outs = {
                            let mut s = session2.lock().unwrap();
                            s.finish_perceive_with_outcome(&job, &outcome)
                        };
                        for out in outs {
                            write_one(&writer2, &out).await;
                        }
                    });
                }
                continue;
            }

            let (responses, version_error) = {
                let mut s = session.lock().unwrap();
                match s.handle(env) {
                    Ok(responses) => (Some(responses), None),
                    Err(SessionError::VersionUnsupported { client, ours }) => {
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
                        (None, Some(out))
                    }
                    Err(SessionError::BadFrame(_))
                    | Err(SessionError::UnknownType(_))
                    | Err(SessionError::UnknownSoul(_)) => {
                        return Ok(());
                    }
                }
            };
            if let Some(out) = version_error {
                write_one(&writer, &out).await;
                return Ok(());
            }
            if let Some(responses) = responses {
                for resp in responses {
                    write_one(&writer, &resp).await;
                }
                if inbound_was_shutdown {
                    return Ok(());
                }
            }
        }
    }
}

async fn write_one(
    writer: &tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>,
    envelope: &Envelope,
) {
    let json = serde_json::to_string(envelope).unwrap_or_default();
    let mut line = json;
    line.push('\n');
    let mut w = writer.lock().await;
    let _ = w.write_all(line.as_bytes()).await;
}

fn parse_envelope(line: &str) -> Result<Envelope, (crate::wire::ErrorCode, String)> {
    let envelope: Result<Envelope, serde_json::Error> = serde_json::from_str(line);
    match envelope {
        Ok(e) => Ok(e),
        Err(_) => Err((
            crate::wire::ErrorCode::BadFrame,
            format!("bad JSON line: {line}"),
        )),
    }
}

pub async fn serve_loopback(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::bind(&addr).await?;
    eprintln!("pan: listening on {addr}");
    loop {
        let stream = server.accept_one().await?;
        tokio::spawn(async move {
            if let Err(e) = Server::drive(stream).await {
                eprintln!("pan: connection error: {e}");
            }
        });
    }
}
