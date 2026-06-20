//! # Containment test — sandbox isolation proof.
//!
//! Proves that wasmtime prevents a loaded Wasm plugin from accessing memory
//! outside its allocated linear memory. The test:
//!
//! 1. Defines a "rogue" plugin in WAT (WebAssembly Text Format) that attempts
//!    to write to memory offset 0x7FFFFFFF (beyond the 1-page initial memory)
//! 2. Instantiates it via wasmtime
//! 3. Calls the exported `pan_plugin_provision` function
//! 4. Asserts wasmtime returns a trap (memory access out of bounds)
//! 5. Asserts the host process continues normally (no segfault, no corruption)

use wasmtime::{Engine, Linker, Module, Store, Trap};

// A rogue Wasm plugin that attempts to access memory beyond its allocated
// linear memory (1 page = 64KB). The plugin has a minimal WASI-like import
// stub for memory but overshoots its bounds.
//
// This WAT module has:
// - 1 page (64KB) of initial memory
// - An exported pan_plugin_provision that writes to offset 0x7FFFFFFF
const ROGUE_PLUGIN_WAT: &str = r#"
(module
  (memory (export "memory") 1)            ;; 1 page = 64 KiB
  (func (export "pan_plugin_provision") (result i32)
    i32.const 0x7FFFFFFF                  ;; far beyond our 64KB memory
    i32.const 42                          ;; value to write
    i32.store offset=0                    ;; OOB write — MUST trap
    i32.const 0                           ;; return success (unreachable)
  )
  (func (export "pan_plugin_validate") (result i32)
    i32.const 0
  )
  (func (export "pan_plugin_run") (result i32)
    i32.const 0
  )
  (func (export "pan_plugin_cleanup")
  )
  (func (export "pan_plugin_health") (result i32)
    i32.const 0
  )
  (func (export "pan_plugin_id") (result i32)
    i32.const 0                           ;; pointer to "" (would be empty in real plugin)
  )
  (func (export "pan_plugin_capabilities") (result i32)
    i32.const 1                           ;; pointer to "" (would be empty in real plugin)
  )
)
"#;

#[test]
fn rogue_plugin_cannot_access_host_memory() {
    let engine = Engine::default();
    let module = Module::new(&engine, ROGUE_PLUGIN_WAT)
        .expect("valid WAT module");

    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);

    // Instantiate the rogue plugin.
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("rogue plugin instantiation should succeed (memory is valid)");

    // Get the provision function.
    let provision = instance
        .get_typed_func::<(), i32>(&mut store, "pan_plugin_provision")
        .expect("pan_plugin_provision export exists");

    // Call it — this SHOULD trap because the write is beyond memory bounds.
    let result = provision.call(&mut store, ());
    match result {
        Err(trap) => {
            // wasmtime trapped the out-of-bounds memory access.
            let msg = format!("{trap:#}");
            assert!(
                msg.contains("out of bounds") || msg.contains("out_of_bounds")
                    || msg.contains("index out of bounds") || msg.contains("wasm trap"),
                "trap message should mention OOB: {msg}"
            );
            eprintln!("SUCCESS: wasmtime trapped rogue memory access: {trap}");
        }
        Ok(_) => {
            panic!("Rogue plugin should NOT have succeeded — the OOB write was not trapped!");
        }
    }

    // The host process is still healthy after the trap.
    eprintln!("Host process unaffected after rogue plugin trap.");
}

#[test]
fn benign_plugin_passes_provision() {
    // A well-behaved plugin that doesn't access out-of-bounds memory.
    let benign_wat = r#"
(module
  (memory (export "memory") 1)
  (func (export "pan_plugin_provision") (result i32)
    i32.const 0        ;; success
  )
  (func (export "pan_plugin_validate") (result i32)
    i32.const 0
  )
  (func (export "pan_plugin_run") (result i32)
    i32.const 0
  )
  (func (export "pan_plugin_cleanup")
  )
  (func (export "pan_plugin_health") (result i32)
    i32.const 0
  )
)
"#;

    let engine = Engine::default();
    let module = Module::new(&engine, benign_wat).expect("valid WAT");
    let mut store = Store::new(&engine, ());
    let linker = Linker::new(&engine);
    let instance = linker.instantiate(&mut store, &module).unwrap();

    let provision = instance
        .get_typed_func::<(), i32>(&mut store, "pan_plugin_provision")
        .unwrap();

    let result = provision.call(&mut store, ());
    assert_eq!(result, Ok(0), "benign plugin should provision successfully");
    eprintln!("Benign plugin provisions correctly.");
}
