//! # Load tests for the daemon Soul Protocol server.
//!
//! These tests require the `pan` binary to be built first (`cargo build -p pan-daemon`).
//! They are gated behind `#[ignore]` so they don't run in normal CI without an
//! explicit opt-in.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Find a free port by binding to port 0 and reading the assigned port.
fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_pan(port: u16) -> std::process::Child {
    let binary = if cfg!(debug_assertions) {
        "target/debug/pan"
    } else {
        "target/release/pan"
    };
    Command::new(binary)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("pan binary to start")
}

/// Send a line over TCP and receive the response.
fn ndjson_talk(stream: &mut TcpStream, line: &str) -> String {
    let mut buf = String::new();
    writeln!(stream, "{line}").unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    reader.read_line(&mut buf).unwrap();
    buf
}

#[test]
#[ignore = "requires pan serve binary"]
fn sustained_perceive_throughput() {
    let port = find_free_port();
    let mut child = spawn_pan(port);

    // Wait for the daemon to start listening.
    std::thread::sleep(Duration::from_millis(500));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // Handshake.
    let resp = ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":0,"re":null,"type":"hello","body":{"hello":{"protocol_version":0,"profile":"reachlock/0","client":"load-test"}}}"#,
    );
    assert!(resp.contains("welcome"), "handshake failed: {resp}");

    // Register capabilities.
    let resp = ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":1,"re":null,"type":"register_capabilities","body":{"register_capabilities":{"capabilities":[]}}}"#,
    );
    assert!(resp.contains("ack"), "register failed: {resp}");

    // Instantiate a rules soul.
    let resp = ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":2,"re":null,"type":"instantiate_soul","body":{"instantiate_soul":{"soul_id":"pilot","mind":"rules","soul":{"rules":[]}}}}"#,
    );
    assert!(resp.contains("ack"), "instantiate failed: {resp}");

    // Send N perceives and measure throughput.
    let n = 50;
    let start = Instant::now();
    for i in 0..n {
        let perceive = format!(
            r#"{{"v":0,"seq":{},"re":null,"type":"perceive","body":{{"perceive":{{"soul_id":"pilot","goal":{{"id":"conv_{i}","revision":1,"objective":"x","trigger":{{"kind":"tick","sequence":{i}}}}}}},"context":{{"fragments":[]}}}}}}"#,
            3 + i
        );
        let resp = ndjson_talk(&mut stream, &perceive);
        assert!(
            resp.contains("decision") || resp.contains("error"),
            "unexpected response: {resp}"
        );
    }
    let elapsed = start.elapsed();
    let throughput = n as f64 / elapsed.as_secs_f64();
    eprintln!("throughput: {throughput:.1} perceives/sec ({n} in {elapsed:.1?})");
    assert!(throughput > 1.0, "throughput too low: {throughput} req/s");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
#[ignore = "requires pan serve binary"]
fn rapid_supersession_stress() {
    let port = find_free_port();
    let mut child = spawn_pan(port);

    std::thread::sleep(Duration::from_millis(500));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // Handshake, register, instantiate.
    ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":0,"re":null,"type":"hello","body":{"hello":{"protocol_version":0,"profile":"reachlock/0","client":"load-test"}}}"#,
    );
    ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":1,"re":null,"type":"register_capabilities","body":{"register_capabilities":{"capabilities":[]}}}"#,
    );
    ndjson_talk(
        &mut stream,
        r#"{"v":0,"seq":2,"re":null,"type":"instantiate_soul","body":{"instantiate_soul":{"soul_id":"pilot","mind":"rules","soul":{"rules":[]}}}}"#,
    );

    // Send many perceives for the same conversation_id (rapid supersession).
    let conv_id = "conv_supersession";
    let n = 20;
    for i in 0..n {
        let perceive = format!(
            r#"{{"v":0,"seq":{},"re":null,"type":"perceive","body":{{"perceive":{{"soul_id":"pilot","goal":{{"id":"{conv_id}","revision":{},"objective":"x","trigger":{{"kind":"tick","sequence":{}}}}},"context":{{"fragments":[]}}}}}}}}"#,
            10 + i,
            i + 1,
            i
        );
        writeln!(stream, "{perceive}").unwrap();
    }

    // Read all responses. The first N-1 should be superseded, the last should
    // be a decision.
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut got_decision = false;
    for _ in 0..n {
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        if buf.contains("decision") {
            got_decision = true;
        }
    }
    assert!(got_decision, "never got a decision response");

    child.kill().unwrap();
    child.wait().unwrap();
}
