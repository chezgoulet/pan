# Pan SDK — Wasm Plugin ABI v0.1.0

The Pan host (plugind) communicates with Wasm plugins through a C ABI.
Plugins can be written in any language that compiles to `wasm32-wasip1`
(or `wasm32-unknown-unknown`) and exports the functions defined in
[`pan_abi.h`](./pan_abi.h).

## ABI Contract

All plugins MUST export the following functions:

| Export | Signature | Phase | Description |
|---|---|---|---|
| `pan_plugin_id` | `() -> *const u8` | Identity | Returns null-terminated UTF-8 string with the plugin's hierarchical id |
| `pan_plugin_capabilities` | `() -> *const u8` | Identity | Returns JSON-encoded capabilities declaration |
| `pan_plugin_provision` | `() -> i32` | Provision | 0 = success, non-zero = error |
| `pan_plugin_validate` | `() -> i32` | Validate | 0 = success, non-zero = error |
| `pan_plugin_run` | `() -> i32` | Run | 0 = success, non-zero = error |
| `pan_plugin_cleanup` | `() -> ()` | Cleanup | Release resources (void) |
| `pan_plugin_health` | `() -> i32` | Health | 0 = healthy, non-zero = degraded |

All string pointers must point to memory valid for the plugin's lifetime
(leaked or static). The host reads them during initialization and will not
call the plugin again after `cleanup`.

## Capabilities JSON format

The `pan_plugin_capabilities` function returns a JSON string:

```json
{
  "provides": ["memory.store", "memory.query"],
  "needs": ["http.client"]
}
```

## Example plugins

- **TinyGo (echo)**: `tinygo-echo/` — minimal plugin that echoes events.
- **Rust (hello)**: `rust-hello/` — minimal plugin that registers a capability.

## Build

### Rust plugin

```bash
cd rust-hello
cargo build --target wasm32-wasip1 --release
```

### TinyGo plugin

```bash
cd tinygo-echo
tinygo build -o plugin.wasm -target=wasi -no-debug .
```

## Containment test

The `containment-test/` crate:
1. Loads a rogue Wasm plugin that attempts to write to arbitrary memory
2. Verifies wasmtime traps the access
3. Proves the sandbox prevents host memory corruption
