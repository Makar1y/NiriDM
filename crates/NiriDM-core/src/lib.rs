//! NiriDM-core: the model, IPC client, and KDL handling shared by the `NiriDM`
//! CLI and the `NiriDM-daemon`.
//!
//! The public surface is small on purpose: everything else in this crate is
//! reachable through the modules below.

mod error;
pub mod ipc;
pub mod kdl;
pub mod layout;
pub mod paths;
pub mod types;

pub use error::{NdError, Result};
pub use ipc::Niri;
pub use paths::Paths;