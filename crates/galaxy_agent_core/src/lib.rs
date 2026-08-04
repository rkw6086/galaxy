//! Provider- and UI-independent contracts for Galaxy agent runtimes.
//!
//! This crate is the stable boundary between Galaxy application services and
//! concrete runtimes such as Rig-backed providers or ACP agents. It must not
//! depend on GalaxyUI, provider SDKs, persistence, or Warp wire protocols.

mod runtime;
mod types;

pub use runtime::*;
pub use types::*;
