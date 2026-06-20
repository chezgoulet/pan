//! Example Rust Wasm plugin for Pan.
//!
//! Exports the standard Pan plugin ABI functions via `extern "C"`.
//!
//! Build:
//! ```bash
//! cargo build --target wasm32-wasip1 --release
//! # or
//! cargo build --target wasm32-unknown-unknown --release
//! ```

#![no_std]

use core::panic::PanicInfo;
use core::slice;
use core::str;

// ---------------------------------------------------------------------------
// Panic handler for wasm32-unknown-unknown (no std)
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

// ---------------------------------------------------------------------------
// Static strings that live for the plugin's lifetime.
// ---------------------------------------------------------------------------

static PLUGIN_ID: &[u8] = b"plugin.hello.rust\x00";
static CAPABILITIES: &[u8] = b"{\"provides\":[\"greet\"],\"needs\":[]}\x00";

// ---------------------------------------------------------------------------
// Exported ABI functions
// ---------------------------------------------------------------------------

/// Returns a pointer to a null-terminated UTF-8 string with the plugin id.
#[no_mangle]
pub extern "C" fn pan_plugin_id() -> *const u8 {
    PLUGIN_ID.as_ptr()
}

/// Returns a pointer to a null-terminated UTF-8 JSON string with capability declarations.
#[no_mangle]
pub extern "C" fn pan_plugin_capabilities() -> *const u8 {
    CAPABILITIES.as_ptr()
}

/// Provision phase. Returns 0 on success.
#[no_mangle]
pub extern "C" fn pan_plugin_provision() -> i32 {
    0
}

/// Validate phase. Returns 0 on success.
#[no_mangle]
pub extern "C" fn pan_plugin_validate() -> i32 {
    0
}

/// Run phase. Returns 0 on success.
#[no_mangle]
pub extern "C" fn pan_plugin_run() -> i32 {
    0
}

/// Cleanup phase.
#[no_mangle]
pub extern "C" fn pan_plugin_cleanup() {}

/// Health probe. Returns 0 if healthy.
#[no_mangle]
pub extern "C" fn pan_plugin_health() -> i32 {
    0
}
