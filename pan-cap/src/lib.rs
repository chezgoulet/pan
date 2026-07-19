//! # pan-cap — concrete capability components.
//!
//! The `cap.*` components a stock agent runs: [`StateCaps`] (`cap.state.*`, an
//! in-memory KV) and [`FsCaps`] (`cap.fs.*`, rooted filesystem access). Each
//! implements pan-core's [`CapabilityProvider`](pan_core::toolbox::CapabilityProvider),
//! so they compose into a [`Toolbox`](pan_core::toolbox::Toolbox) that becomes
//! both the pipeline's capability registry and its executor.
//!
//! This is the layer that lets an assembled agent actually *do* things: the
//! governor decides *whether* a persona may reach `cap.fs`; these components are
//! *what runs* when it may (with their own defense-in-depth, e.g. the fs jail).
//!
//! ```
//! use pan_core::toolbox::Toolbox;
//! use pan_cap::{FsCaps, StateCaps};
//!
//! let toolbox = Toolbox::new()
//!     .with(Box::new(StateCaps::new())).unwrap()
//!     .with(Box::new(FsCaps::new("/var/lib/pan/agent-root"))).unwrap();
//! // toolbox.registry() -> the pipeline's CapabilityRegistry;
//! // &toolbox           -> the pipeline's Executor.
//! ```

pub mod fs;
pub mod shell;
pub mod state;

pub use fs::FsCaps;
pub use shell::ShellCaps;
pub use state::StateCaps;

use pan_core::components::{ComponentError, ComponentRegistry};

/// Register the capability components this crate provides into `registry`, so an
/// `Agent.toml` `[caps.enable]` list can build them by id.
///
/// - `cap.state` takes no settings.
/// - `cap.fs` **requires** a `root` setting (its jail directory) — omitting it is
///   a load-time error, not a silent unrooted filesystem.
pub fn register_builtin_caps(registry: &mut ComponentRegistry) -> Result<(), ComponentError> {
    registry.register_capability_provider("cap.state", |_cfg| Ok(Box::new(StateCaps::new())))?;
    registry.register_capability_provider("cap.fs", |cfg| {
        let root = cfg
            .settings
            .get("root")
            .and_then(|r| r.as_str())
            .ok_or_else(|| ComponentError::Construction {
                id: cfg.id.clone(),
                reason: "cap.fs requires a `root` setting".into(),
            })?;
        Ok(Box::new(FsCaps::new(root)))
    })?;
    registry.register_capability_provider("cap.shell", |_cfg| Ok(Box::new(ShellCaps::new())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::components::ComponentConfig;

    #[test]
    fn builtin_caps_register_and_build() {
        let mut reg = ComponentRegistry::new();
        register_builtin_caps(&mut reg).unwrap();
        let ids: Vec<&str> = reg.capability_ids().collect();
        assert!(ids.contains(&"cap.state"));
        assert!(ids.contains(&"cap.fs"));

        // cap.fs needs a root.
        let no_root = reg.build_capability_provider(&ComponentConfig::bare("cap.fs"));
        assert!(no_root.is_err(), "cap.fs without a root must fail to build");

        let rooted = reg.build_capability_provider(&ComponentConfig::new(
            "cap.fs",
            serde_json::json!({ "root": "/tmp/whatever" }),
        ));
        assert!(rooted.is_ok());
    }
}
