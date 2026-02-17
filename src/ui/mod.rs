pub mod tree;

#[cfg(target_os = "macos")]
pub mod accessibility;

#[cfg(target_os = "macos")]
pub mod capture;

// COMP-3: Linux accessibility stub (AT-SPI not yet implemented)
#[cfg(target_os = "linux")]
mod accessibility_linux;

#[cfg(target_os = "linux")]
pub use accessibility_linux as accessibility;

pub mod vision;
pub mod merge;
