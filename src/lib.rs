//! # bidi-browser-lite
//!
//! Full lifecycle control of WebDriver BiDi browsers (Firefox in 0.1): find
//! the binary, launch a managed instance or attach to a running one, allocate
//! its ports, prepare its profile, own its single BiDi session and stop it
//! cleanly. It is the BiDi twin of `cdp-browser-lite` and hands out
//! [`bidi_lite`] clients already connected to the browser.
//!
//! Work in progress.

pub mod browser;
pub mod config;
pub mod discovery;
pub mod error;
mod launcher;
pub mod pool;
pub mod ports;
pub mod probe;
pub mod process;
pub mod profile;
#[cfg(feature = "test-support")]
pub mod test_support;
