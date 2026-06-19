pub mod parse;

#[cfg(all(windows, feature = "app"))]
mod worker;

#[cfg(all(windows, feature = "app"))]
pub use worker::sim_worker;

#[cfg(any(not(windows), not(feature = "app")))]
mod stub;

#[cfg(any(not(windows), not(feature = "app")))]
pub use stub::sim_worker;
