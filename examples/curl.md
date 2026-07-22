# Gateway — curl examples

Start the gateway:

```sh
pan-gateway --agents-dir ./examples/agents --port 40707
```

## Basic chat

```sh
curl -s http://127.0.0.1:40707/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "echo",
    "messages": [{"role": "user", "content": "Hello!"}]
  }' | jq
```

## Streaming chat

```sh
curl -s --no-buffer http://127.0.0.1:40707/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "echo",
    "messages": [{"role": "user", "content": "Tell me a story."}],
    "stream": true
  }'
```

## Agent goals (Pan-native)

```sh
curl -s http://127.0.0.1:40707/v1/agents/echo/goals \
  -H 'Content-Type: application/json' \
  -d '{"objective": "Do something interesting"}' | jq
```

## List agents

```sh
curl -s http://127.0.0.1:40707/v1/agents | jq
```

## Health check

```sh
curl -s http://127.0.0.1:40707/health | jq
```

## Metrics

```sh
curl -s http://127.0.0.1:40707/v1/metrics | jq
```

## Agent delegation

```sh
# Start with two agents loaded, then delegate from parent to child:
curl -s http://127.0.0.1:40707/v1/agents/echo/delegate \
  -H 'Content-Type: application/json' \
  -d '{
    "agent": "helper",
    "objective": "What is the weather?"
  }' | jq
```

## With auth token

```sh
pan-gateway --agents-dir ./examples/agents --port 40707 --auth-token my-secret

curl -s http://127.0.0.1:40707/v1/health \
  -H 'Authorization: Bearer my-secret' | jq
```
