//! Tom OS Installer: plan, disks, catalogues and the root backend (the GUI lives in `gui`).

pub mod backend;
pub mod catalog;
pub mod disks;
pub mod events;
pub mod plan;

#[cfg(feature = "gui")]
pub mod gui;


pub const VERSION: &str = env!("CARGO_PKG_VERSION");
