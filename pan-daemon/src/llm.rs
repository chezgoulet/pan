//! # The LLM mind — an OpenAI-compatible chat provider (M2).
//!
//! Implements the same [`Provider`] trait as the rules mind: `Goal` +
//! `Context` + capabilities in, `Decision` out. Everything chat-shaped —
//! endpoints, prompts, models — stays private to this module; that is the
//! whole point of the vocabulary (see pan-core `providers.rs`).
//!
//! v0 targets **local, plain-HTTP** OpenAI-compatible servers (Ollama,
//! llama.cpp, LM Studio) with a deliberately tiny std-only HTTP/1.0 client:
//! no TLS, no async, no new dependencies. HTTP/1.0 sidesteps chunked
//! transfer-encoding, so the client is ~a page of honest code. Cloud BYOK
//! (Anthropic/OpenAI over TLS) is a later, additive provider behind the same
//! trait — it needs a TLS dependency this crate doesn't take lightly.
//!
//! Config (environment):
//! - `PAN_LLM_BASE`  — base URL (e.g. `http://127.0.0.1:11434` for Ollama).
//!   **Unset = llm mind disabled.** Explicit opt-in keeps test runs
//!   deterministic regardless of what happens to be listening locally.
//! - `PAN_LLM_MODEL` — model id; default: first id from `GET /v1/models`.
//!
//! At daemon start [`resolve`] probes the endpoint once; if unreachable, the
//! daemon simply doesn't advertise the `llm` mind and llm-minded souls fall
//! back to a Continue-only decision. The game must always run without a model.
//!
//! Known limitation (v0): `decide` blocks the session loop for the duration
//! of one completion. One slow soul delays the next perceive on the same
//! connection. Per-soul worker threads are the M2.1 follow-up.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::time::Duration;

use pan_core::schema::{ActionIntent, Capability, Context, Decision, Goal, Outcome, Provider, Trigger};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_COMPLETION_TOKENS: u32 = 200;

/// Which dialect the endpoint speaks. Auto-detected at resolve time: servers
/// answering Ollama's native `/api/version` get the native API (the only one
/// whose `think: false` is honored reliably); everything else gets
/// OpenAI-compatible `/v1/chat/completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    OpenAiCompat,
    OllamaNative,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub host: String,
    pub port: u16,
    pub model: String,
    pub api: ApiKind,
}

/// Probe the configured endpoint once per process and cache the outcome.
pub fn resolve() -> Option<&'static LlmConfig> {
    static RESOLVED: OnceLock<Option<LlmConfig>> = OnceLock::new();
    RESOLVED.get_or_init(resolve_uncached).as_ref()
}

fn resolve_uncached() -> Option<LlmConfig> {
    let Ok(base) = std::env::var("PAN_LLM_BASE") else {
        eprintln!(
            "pan llm: PAN_LLM_BASE unset — llm mind disabled \
             (for Ollama: PAN_LLM_BASE=http://127.0.0.1:11434)"
        );
        return None;
    };
    let (host, port) = match parse_http_base(&base) {
        Ok(hp) => hp,
        Err(e) => {
            eprintln!("pan llm: PAN_LLM_BASE {base:?}: {e} — llm mind disabled");
            return None;
        }
    };
    let model = match std::env::var("PAN_LLM_MODEL") {
        Ok(m) if !m.is_empty() => m,
        _ => match first_model(&host, port) {
            Some(m) => m,
            None => {
                eprintln!("pan llm: no server at {host}:{port} (or no models) — llm mind disabled");
                return None;
            }
        },
    };
    let api = if http_request(&host, port, "GET", "/api/version", None, PROBE_TIMEOUT).is_ok() {
        ApiKind::OllamaNative
    } else {
        ApiKind::OpenAiCompat
    };
    eprintln!("pan llm: {host}:{port} model {model} ({api:?})");
    Some(LlmConfig { host, port, model, api })
}

/// `http://host[:port]` only. https is a deliberate error: this client has no
/// TLS on purpose — point PAN_LLM_BASE at a local OpenAI-compatible server.
fn parse_http_base(base: &str) -> Result<(String, u16), String> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// bases are supported (local inference)".to_string())?;
    let rest = rest.trim_end_matches('/');
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("bad port {p:?}"))?),
        None => (rest, 80),
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    Ok((host.to_string(), port))
}

fn first_model(host: &str, port: u16) -> Option<String> {
    let response = http_request(host, port, "GET", "/v1/models", None, PROBE_TIMEOUT).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&response).ok()?;
    parsed["data"][0]["id"].as_str().map(|s| s.to_string())
}

/// Fire a small completion so the server loads the model before the first
/// real line of dialogue needs it. Fire-and-forget on a thread.
pub fn warm_up(config: &LlmConfig) {
    let config = config.clone();
    std::thread::spawn(move || {
        let messages = serde_json::json!([{"role": "user", "content": "Say the single word: ready"}]);
        let started = std::time::Instant::now();
        match chat(&config, &messages, 2) {
            Ok(_) => eprintln!("pan llm: warm-up ok ({} ms)", started.elapsed().as_millis()),
            Err(e) => eprintln!("pan llm: warm-up failed: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

pub struct LlmProvider {
    pub config: LlmConfig,
}

impl Provider for LlmProvider {
    fn id(&self) -> &str {
        "provider.llm.openai_compatible"
    }

    /// Chat-shaped mapping, private to this impl: fragments become the system
    /// prompt, the trigger becomes the user turn, the reply becomes `Express`.
    /// A transport/parse failure becomes `Conclude(Abandoned)` — the host's
    /// dialogue layer treats that as "the moment passes" and falls back.
    fn decide(&self, goal: &Goal, ctx: &Context, _caps: &[Capability]) -> Decision {
        let messages = serde_json::json!([
            {"role": "system", "content": system_prompt(goal, ctx)},
            {"role": "user", "content": user_turn(goal)},
        ]);
        match chat(&self.config, &messages, MAX_COMPLETION_TOKENS) {
            Ok(raw) => {
                let line = clean_line(&raw);
                if line.is_empty() {
                    abandoned("empty completion")
                } else {
                    Decision {
                        intents: vec![
                            ActionIntent::Express { body: line },
                            ActionIntent::Conclude { outcome: Outcome::Achieved },
                        ],
                    }
                }
            }
            Err(e) => abandoned(&e),
        }
    }
}

/// Small local models narrate stage directions and trail off mid-sentence at
/// the token cap. Keep the spoken words: drop leading `(…)`/`*…*` blocks,
/// unwrap quotation marks, and cut a truncated tail back to the last
/// sentence end.
fn clean_line(raw: &str) -> String {
    let mut text = raw.trim();
    loop {
        text = text.trim_start();
        let (open, close) = match text.chars().next() {
            Some('(') => ('(', ')'),
            Some('*') => ('*', '*'),
            _ => break,
        };
        match text[1..].find(close) {
            Some(end) => text = &text[1 + end + close.len_utf8()..],
            None => break,
        }
        let _ = open;
    }
    let mut line = text.trim().trim_matches('"').trim().to_string();
    if !line.is_empty() && !line.ends_with(['.', '!', '?', '…', '"', '\'']) {
        if let Some(cut) = line.rfind(['.', '!', '?', '…']) {
            if cut >= 20 {
                line.truncate(cut + line[cut..].chars().next().map_or(1, |c| c.len_utf8()));
            }
        }
    }
    line
}

fn abandoned(reason: &str) -> Decision {
    eprintln!("pan llm: decide failed: {reason}");
    Decision { intents: vec![ActionIntent::Conclude { outcome: Outcome::Abandoned }] }
}

fn system_prompt(goal: &Goal, ctx: &Context) -> String {
    let mut sections: Vec<String> = Vec::new();
    for fragment in &ctx.fragments {
        // Channel headers keep the small models oriented; the persona channel
        // leads because context assembly orders it first (host contract).
        sections.push(format!("[{}]\n{}", fragment.channel, fragment.body));
    }
    sections.push(format!(
        "[direction]\n{}\nAnswer with your character's next spoken line only — \
         no stage directions, no quotation marks, no name prefix.",
        goal.objective
    ));
    sections.join("\n\n")
}

fn user_turn(goal: &Goal) -> String {
    match &goal.trigger {
        Trigger::Utterance { from, content } => format!("{from} says: {content}"),
        Trigger::Event { topic, payload } => {
            format!("(something happens: {topic} {payload})")
        }
        Trigger::Tick { .. } => "(a quiet moment passes)".to_string(),
        Trigger::Signal { name, value } => format!("(reading: {name} = {value})"),
    }
}

/// One chat completion, returning the assistant's raw text. Dialect
/// differences (path, token-cap key, response shape) stay inside this fn.
fn chat(config: &LlmConfig, messages: &serde_json::Value, max_tokens: u32) -> Result<String, String> {
    let (path, body) = match config.api {
        ApiKind::OllamaNative => (
            "/api/chat",
            serde_json::json!({
                "model": config.model,
                "messages": messages,
                "stream": false,
                // Reasoning models burn the whole budget "thinking" and return
                // empty content; the native API is where Ollama honors this.
                "think": false,
                "options": {"num_predict": max_tokens, "temperature": 0.8},
            }),
        ),
        ApiKind::OpenAiCompat => (
            "/v1/chat/completions",
            serde_json::json!({
                "model": config.model,
                "messages": messages,
                "max_tokens": max_tokens,
                "temperature": 0.8,
                "think": false,  // ignored by servers that don't know it
            }),
        ),
    };
    let response = http_request(
        &config.host, config.port, "POST", path, Some(&body.to_string()), HTTP_TIMEOUT,
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&response).map_err(|e| format!("bad completion JSON: {e}"))?;
    if let Some(err) = parsed.get("error") {
        return Err(format!("server error: {err}"));
    }
    let content = match config.api {
        ApiKind::OllamaNative => parsed["message"]["content"].as_str(),
        ApiKind::OpenAiCompat => parsed["choices"][0]["message"]["content"].as_str(),
    };
    Ok(content.unwrap_or("").to_string())
}

// ---------------------------------------------------------------------------
// The tiny HTTP/1.0 client
// ---------------------------------------------------------------------------

/// One HTTP/1.0 exchange. 1.0 means the server neither keeps the connection
/// alive nor chunk-encodes: read to EOF, split head from body, done.
fn http_request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    json_body: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;

    let body = json_body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).map_err(|e| format!("send: {e}"))?;

    let mut raw = String::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut raw)
        .map_err(|e| format!("read: {e}"))?;

    let (head, response_body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    if status != 200 {
        return Err(format!("HTTP {status}: {}", &response_body[..response_body.len().min(300)]));
    }
    Ok(response_body.to_string())
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot fake OpenAI-compatible server: accepts one connection,
    /// returns `payload` as an HTTP/1.0 200 (or the given status).
    fn fake_server(status: u16, payload: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf); // one read is enough for tests
            let reason = if status == 200 { "OK" } else { "ERR" };
            let response = format!(
                "HTTP/1.0 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                payload.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        port
    }

    fn goal_utterance() -> Goal {
        Goal {
            id: "g1".into(),
            revision: 1,
            objective: "Respond in character.".into(),
            trigger: Trigger::Utterance { from: "player".into(), content: "You alright?".into() },
        }
    }

    fn config(port: u16) -> LlmConfig {
        LlmConfig {
            host: "127.0.0.1".into(),
            port,
            model: "test-model".into(),
            api: ApiKind::OpenAiCompat,
        }
    }

    #[test]
    fn completion_becomes_express_then_conclude_achieved() {
        let port = fake_server(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"  Been through worse. "}}]}"#,
        );
        let provider = LlmProvider { config: config(port) };
        let decision = provider.decide(&goal_utterance(), &Context::default(), &[]);
        assert_eq!(
            decision.intents,
            vec![
                ActionIntent::Express { body: "Been through worse.".into() },
                ActionIntent::Conclude { outcome: Outcome::Achieved },
            ]
        );
    }

    #[test]
    fn http_error_becomes_conclude_abandoned() {
        let port = fake_server(500, r#"{"error":"boom"}"#);
        let provider = LlmProvider { config: config(port) };
        let decision = provider.decide(&goal_utterance(), &Context::default(), &[]);
        assert_eq!(decision.outcome(), Some(Outcome::Abandoned));
        assert!(decision.intents.iter().all(|i| !matches!(i, ActionIntent::Express { .. })));
    }

    #[test]
    fn unreachable_server_becomes_conclude_abandoned() {
        // Port 1 is virtually never listening on loopback.
        let provider = LlmProvider { config: config(1) };
        let decision = provider.decide(&goal_utterance(), &Context::default(), &[]);
        assert_eq!(decision.outcome(), Some(Outcome::Abandoned));
    }

    #[test]
    fn system_prompt_keeps_channel_order_and_direction() {
        let ctx = Context::default()
            .with("persona", "You are the pilot.")
            .with("memory", "You remember the ambush.");
        let prompt = system_prompt(&goal_utterance(), &ctx);
        let persona_at = prompt.find("[persona]").unwrap();
        let memory_at = prompt.find("[memory]").unwrap();
        let direction_at = prompt.find("[direction]").unwrap();
        assert!(persona_at < memory_at && memory_at < direction_at);
        assert!(prompt.contains("spoken line only"));
    }

    #[test]
    fn ollama_native_shape_is_understood() {
        let port = fake_server(
            200,
            r#"{"message":{"role":"assistant","content":"Steady as she goes."},"done":true}"#,
        );
        let mut cfg = config(port);
        cfg.api = ApiKind::OllamaNative;
        let provider = LlmProvider { config: cfg };
        let decision = provider.decide(&goal_utterance(), &Context::default(), &[]);
        assert_eq!(
            decision.intents[0],
            ActionIntent::Express { body: "Steady as she goes.".into() }
        );
    }

    #[test]
    fn clean_line_strips_stage_directions_and_truncation() {
        assert_eq!(
            clean_line("(My grip tightens on the yoke.)\n\nSave the sentiment."),
            "Save the sentiment."
        );
        assert_eq!(
            clean_line("*looks away*  \"Don't make it a thing.\""),
            "Don't make it a thing."
        );
        // A truncated tail is cut back to the last complete sentence.
        assert_eq!(
            clean_line("You took that hit for me back there. I won't forget the way you, uh"),
            "You took that hit for me back there."
        );
        // Short fragments without terminal punctuation survive untouched
        // (the cut-back only applies past the 20-char guard).
        assert_eq!(clean_line("Peachy"), "Peachy");
        assert_eq!(clean_line("   "), "");
    }

    #[test]
    fn base_url_parsing() {
        assert_eq!(parse_http_base("http://127.0.0.1:11434").unwrap(), ("127.0.0.1".into(), 11434));
        assert_eq!(parse_http_base("http://localhost").unwrap(), ("localhost".into(), 80));
        assert!(parse_http_base("https://api.example.com").is_err());
        assert!(parse_http_base("ftp://x").is_err());
    }
}
