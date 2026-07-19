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
pub mod state;

pub use fs::FsCaps;
pub use state::StateCaps;
