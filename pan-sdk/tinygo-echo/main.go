//go:build wasm || tinygo.wasm

package main

// Simple WASM plugin in TinyGo.
// Exports the standard Pan plugin ABI functions.
// Compile: tinygo build -o plugin.wasm -target=wasi -no-debug .
//
// String pointers returned by identity/capabilities functions point to
// package-level buffers that are valid for the plugin's lifetime.

var (
	// Null-terminated strings for ABI returns.
	pluginIDStr        = "plugin.echo.tinygo\x00"
	capabilitiesJSON   = `{"provides":["echo"],"needs":[]}` + "\x00"
)

//go:export pan_plugin_id
func panPluginID() *byte {
	return &[]byte(pluginIDStr)[0]
}

//go:export pan_plugin_capabilities
func panPluginCapabilities() *byte {
	return &[]byte(capabilitiesJSON)[0]
}

//go:export pan_plugin_provision
func panPluginProvision() int32 {
	return 0
}

//go:export pan_plugin_validate
func panPluginValidate() int32 {
	return 0
}

//go:export pan_plugin_run
func panPluginRun() int32 {
	return 0
}

//go:export pan_plugin_cleanup
func panPluginCleanup() {}

//go:export pan_plugin_health
func panPluginHealth() int32 {
	return 0
}

// main is required for TinyGo but is a no-op for Wasm plugins.
func main() {}
