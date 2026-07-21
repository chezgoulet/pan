//! End-to-end proof that `provider.llm` *uses* a tool through pan-core's ReAct
//! loop: a scripted mock server first returns a `tool_calls` reply, the loop
//! executes the governed capability and folds the result back, and on the second
//! turn the model — now seeing the result in its reconstructed transcript —
//! answers and concludes. No network, no key: the whole cycle runs against a
//! localhost mock.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pan_core::events::{EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once, RunEnd};
use pan_core::pipeline::{AllowAll, ExecError, Executor, Pipeline};
use pan_core::registry::CapabilityRegistry;
use pan_core::schema::{Capability, Context, Goal, Outcome, Scope, Trigger, Value};

use pan_llm::OpenAiProvider;

/// An executor that answers `cap.compute` with a fixed structured result.
struct ComputeExecutor;
#[async_trait::async_trait]
impl Executor for ComputeExecutor {
    fn id(&self) -> &str {
        "exec.compute"
    }
    async fn execute(&self, _capability: &str, _args: &Value) -> Result<Value, ExecError> {
        Ok(serde_json::json!({ "value": 42 }))
    }
}

/// A mock server that serves a scripted queue of JSON bodies over successive
/// HTTP/1.0 connections, capturing each request it received for assertions.
struct MockServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

fn spawn_mock(responses: Vec<String>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    std::thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            captured.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    MockServer { port, requests }
}

/// Read a full HTTP request (headers + Content-Length body) into a String.
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        if let Some(headers_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..headers_end]);
            let content_length = head
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                })
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= headers_end + 4 + content_length {
                break;
            }
        }
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn the_model_calls_a_tool_sees_the_result_and_answers() {
    // Turn 1: the model asks to call the tool. Turn 2 (after the loop feeds the
    // result back): it answers in plain text.
    let mock = spawn_mock(vec![
        serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "cap_compute", "arguments": "{\"x\":6,\"y\":7}" }
                    }]
                }
            }]
        })
        .to_string(),
        serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "The answer is 42." } }]
        })
        .to_string(),
    ]);

    let mut registry = CapabilityRegistry::new();
    registry
        .register(Capability {
            id: "cap.compute".into(),
            summary: "multiply two numbers".into(),
            args_schema: serde_json::json!({ "type": "object" }),
        })
        .unwrap();

    let mut stream = EventStream::spawn(MemorySink::new());
    let pipeline = Pipeline {
        registry: &registry,
        governor: &AllowAll,
        executor: &ComputeExecutor,
        events: &stream,
    };

    let provider = OpenAiProvider {
        base: format!("http://127.0.0.1:{}", mock.port),
        model: "test-model".into(),
        api_key: None,
        instruction: "You are a calculator.".into(),
        max_tokens: 64,
        temperature: 0.0,
        token_budget: None,
        tokens_used: std::sync::atomic::AtomicU64::new(0),
    };

    let lp = Loop {
        provider: &provider,
        pipeline: &pipeline,
        events: &stream,
        scope: Scope::system(),
    };

    let goal = Goal {
        id: "q".into(),
        revision: 0,
        objective: "Answer the user.".into(),
        trigger: Trigger::Utterance {
            from: "user".into(),
            content: "what is 6 times 7?".into(),
        },
    };
    let mut obs = Once(Some(goal));
    let report = lp.run_span(&mut obs, &Context::default()).await;

    // The governed capability ran exactly once, the final answer was expressed,
    // and the span concluded.
    assert_eq!(report.effected, vec!["cap.compute"]);
    assert_eq!(report.expressed, vec!["The answer is 42."]);
    assert_eq!(report.end, Some(RunEnd::Concluded(Outcome::Achieved)));

    // Two round-trips to the model, and the second one carried the reconstructed
    // tool exchange — proof the loop fed the result back and the stateless
    // provider rebuilt the transcript.
    let requests = mock.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "one call to request the tool, one to answer"
    );
    assert!(
        requests[0].contains("cap_compute"),
        "turn 1 must advertise the tool schema"
    );
    assert!(
        requests[1].contains("call_1") && requests[1].contains("42"),
        "turn 2 must replay the assistant tool_call (call_1) and its result (42)"
    );

    stream.shutdown();
}
