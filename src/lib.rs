pub mod hid;
pub mod log;
pub mod rumble;
pub mod sim;
pub mod types;

#[cfg(all(windows, feature = "app"))]
pub mod tray;
#[cfg(all(windows, feature = "app"))]
pub mod ui;
#[cfg(all(windows, feature = "app"))]
pub mod updater;

pub use log::LogBuffer;
pub use types::*;
pub use rumble::RumbleEngine; // Делаем структуру доступной для worker.rs через crate::RumbleEngine