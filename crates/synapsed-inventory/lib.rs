//! Passive, read-only host hardware inventory and topology derivation.

pub mod model;
pub mod topology;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub use model::{
    Bus, DeviceBinding, DriverCatalogEntry, InventoryMatch, InventoryRole, RawRecord,
    SourceCollection,
};
pub use topology::{build, build_with_sources};
