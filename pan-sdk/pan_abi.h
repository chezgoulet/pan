/// Pan Plugin ABI v0.1.0 — C header for Wasm plugin authors.
///
/// Implementations of this ABI shall export the following functions with
/// `extern "C"` (Rust), `//go:export` (TinyGo), or equivalent.
/// Return 0 for success, non-zero for error on int-returning functions.
/// String pointers must point to null-terminated UTF-8 data valid for the
/// plugin's entire lifetime (static or leaked, never stack-allocated).

#ifndef PAN_ABI_H
#define PAN_ABI_H

#ifdef __cplusplus
extern "C" {
#endif

/// Plugin identity. Returns a null-terminated UTF-8 string with the
/// plugin's hierarchical id (e.g. "provider.llm.anthropic").
extern const char* pan_plugin_id(void);

/// Capability declaration. Returns a null-terminated UTF-8 JSON string:
///   {"provides":["cap.a"],"needs":["cap.b"]}
extern const char* pan_plugin_capabilities(void);

/// Lifecycle: provision phase. Called once at startup.
extern int pan_plugin_provision(void);

/// Lifecycle: validate phase. Last chance to refuse before running.
extern int pan_plugin_validate(void);

/// Lifecycle: run phase. Enter active state.
extern int pan_plugin_run(void);

/// Lifecycle: cleanup phase. Release resources.
extern void pan_plugin_cleanup(void);

/// Health probe. Return 0 if healthy, non-zero if degraded.
extern int pan_plugin_health(void);

#ifdef __cplusplus
}
#endif

#endif // PAN_ABI_H
