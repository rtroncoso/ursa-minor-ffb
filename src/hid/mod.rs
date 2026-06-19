pub mod protocol;

#[cfg(windows)]
mod win32;
#[cfg(all(windows, feature = "app"))]
mod worker;

#[cfg(all(windows, feature = "app"))]
pub use worker::hid_worker;

#[cfg(any(not(windows), not(feature = "app")))]
mod stub;

#[cfg(any(not(windows), not(feature = "app")))]
pub use stub::hid_worker;
