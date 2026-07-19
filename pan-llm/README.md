# pan-llm — tool-using LLM providers

The brain that makes an assembled agent *intelligent* — and, crucially,
*tool-using*. `provider.llm` is an ordinary [`Provider`](../pan-core/src/schema.rs):
`Goal` + `Context` + capabilities in, `Decision` out. Nothing chat-shaped leaks
into the core; the model, the messages, and the function schema all stay inside
this crate.

## How it uses tools (the ReAct cycle)

It is built to ride pan-core's [ReAct loop](../pan-core/src/loop_engine.rs):

1. The agent's capabilities become the request's `tools` (OpenAI function schema;
   `cap.state.get` is sent as `cap_state_get` and mapped back on the way in).
2. A model `tool_calls` reply becomes `Invoke` intents — one per call, the
   `tool_call` id carried as the intent's `correlation`, and **no `Conclude`**.
3. The loop runs each `Invoke` through the governed pipeline, folds the results
   onto `TOOL_RESULT_CHANNEL`, and calls `decide` again.
4. Seeing the results, the model answers in plain text → `Express` +
   `Conclude(Achieved)`.

The provider is **stateless** across those turns: it reconstructs the full
function-calling transcript (system, user, then each `assistant(tool_call)` →
`tool(result)` pair) from the goal and the `tool_result` fragments the loop
accumulated. That is why the core records each call's originating `args` in the
fragment — the assistant turn cannot be rebuilt without them. A cancelled
(superseded) `decide` therefore leaves no conversation state behind.

## Use it from `Agent.toml`

`provider.llm` is registered in `pan-agent`'s built-in set, so any manifest can
select it:

```toml
[persona]
provider = "provider.llm"
instruction = "You are a helpful assistant."
model = "llama3.2"
base = "http://127.0.0.1:11434/v1"   # Ollama, llama.cpp, LM Studio, a gateway…
# optional: max_tokens, temperature, api_key
```

`base`/`model`/`api_key` also fall back to `PAN_LLM_BASE` / `PAN_LLM_MODEL` /
`PAN_LLM_API_KEY`, so secrets need not live in the manifest. A missing `base` or
`model` is a **load-time error** — an LLM persona with nowhere to reach is a
misconfiguration, not a silent no-op.

## Transport

The client (`src/http.rs`) is a tiny std-only, blocking **HTTP/1.0** client that
speaks over either transport, chosen by the `base` scheme:

- **`http://`** — a plain `TcpStream`, for local OpenAI-compatible servers and the
  test mock.
- **`https://`** — a rustls TLS stream (pure-Rust: the `ring` provider — no
  cmake/C toolchain — and the Mozilla root set via `webpki-roots`), for cloud BYOK.
  `api_key` is sent as `Authorization: Bearer …`.

The request/response handling is identical across both; only the byte transport
differs. HTTP/1.0 is deliberate (shared with `pan-daemon`'s llm client): it tells
the server not to keep-alive or chunk-encode, so "read to EOF, split head from
body" is a correct, tiny parser. A TLS peer that closes without a `close_notify`
surfaces `UnexpectedEof`, which is treated as a clean end once the body is in hand.

Blocking I/O inside `async fn` is deliberate too: the loop's abandon-path gives
cancellation at the future level (a superseded goal drops the whole `decide`). A
non-blocking client is a later refinement.

The `https` path is exercised live — against a real endpoint — by
`tests/live_cloud.rs`, which is **credential-gated** (a no-op unless
`PAN_LLM_BASE` / `PAN_LLM_MODEL` / `PAN_LLM_API_KEY` are set), so CI and offline
runs skip it. A separate **Anthropic-native** message dialect (vs the
OpenAI-compatible one here) is an optional sibling provider.

## Run it

```sh
cargo test -p pan-llm
```

The tests need no network or key: unit tests cover schema mapping, transcript
reconstruction, and response interpretation, and `tests/tool_use.rs` drives the
**whole ReAct cycle** — model asks for a tool, the loop executes it, the model
sees the result and answers — against a localhost mock server.
