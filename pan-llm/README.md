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

Today the client (`src/http.rs`) is a tiny std-only, blocking **HTTP/1.0** client
— plain HTTP, aimed at local OpenAI-compatible servers. An `https://` base is a
clear early error, not a silent plaintext downgrade. Cloud BYOK over TLS is an
additive transport behind the same `post_json` shape (the next increment); the
tool-use *mapping* above is unchanged by it.

Blocking I/O inside `async fn` is deliberate and matches `pan-daemon`'s llm
client: the loop's abandon-path gives cancellation at the future level (a
superseded goal drops the whole `decide`). A non-blocking client is a later
refinement.

## Run it

```sh
cargo test -p pan-llm
```

The tests need no network or key: unit tests cover schema mapping, transcript
reconstruction, and response interpretation, and `tests/tool_use.rs` drives the
**whole ReAct cycle** — model asks for a tool, the loop executes it, the model
sees the result and answers — against a localhost mock server.
