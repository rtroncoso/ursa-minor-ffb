pub mod hid;
pub mod log;
pub mod preset;
pub mod rumble;
pub mod sim;
pub mod types;

#[cfg(all(windows, feature = "app"))]
pub mod tray;
#[cfg(all(windows, feature = "app"))]
pub mod ui;
#[cfg(all(windows, any(feature = "app", feature = "updater")))]
pub mod updater;

pub use log::LogBuffer;
pub use preset::{
    AppSettings, LayoutField, Preset, PresetKind, PresetShared, PresetStore, SimVarLayout,
    SimVarProfile,
};
pub use types::*;
