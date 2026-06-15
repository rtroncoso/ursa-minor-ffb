pub mod protocol;

#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use worker::hid_worker;

#[cfg(not(windows))]
mod stub;

#[cfg(not(windows))]
pub use stub::hid_worker;
