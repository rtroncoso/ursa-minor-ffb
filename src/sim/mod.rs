pub mod parse;

#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use worker::sim_worker;

#[cfg(not(windows))]
mod stub;

#[cfg(not(windows))]
pub use stub::sim_worker;
