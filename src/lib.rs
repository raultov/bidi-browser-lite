//! # bidi-browser-lite
//!
//! Full lifecycle control of WebDriver BiDi browsers (Firefox in 0.1) on top
//! of [`bidi_lite`]: find the binary, launch a managed instance or attach to
//! a running one, allocate its ports, prepare its profile, own its single
//! BiDi session, hand out `bidi-lite` clients, and stop it cleanly. It is the
//! BiDi twin of `cdp-browser-lite`: same type and method names where the
//! concept is the same.
//!
//! ```no_run
//! use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode, ReadinessState};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = BrowserConfig::builder().mode(LaunchMode::LaunchNew).build();
//! let browser = Browser::ensure(config).await?;
//! let tab = browser.new_tab("about:blank").await?;
//! tab.navigate("https://www.rust-lang.org", ReadinessState::Complete).await?;
//! browser.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! **One session per browser.** Firefox allows a single BiDi session and
//! keeps it after its WebSocket dies, so a [`Browser`] owns exactly one and
//! every client it hands out shares it. Call [`Browser::stop`] when done: for
//! an attached Firefox it is the only way to free the session.
//!
//! **RDP is opt-in.** [`BrowserConfigBuilder::rdp`] enables Firefox's
//! debugger server for [`Browser::rdp_client`]; while enabled, any local
//! process reaching that port can drive the browser with chrome privileges.
//!
//! Feature `test-support` adds in-process BiDi/RDP mocks
//! ([`test_support`]) and the `fake_firefox_helper` binary for tests without
//! a real Firefox.

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
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use bidi_lite;
pub use bidi_lite::browser::{BrowserClient, ContextInfo, ReadinessState};
pub use bidi_lite::client::BidiClient;
pub use bidi_lite::error::{BidiError, BidiResult};
pub use bidi_lite::event_filter::EventFilter;
pub use bidi_lite::protocol::{NoParams, WsCommand, WsResponse};
pub use bidi_lite::rdp::client::RdpClient;
pub use bidi_lite::rdp::debugger::Debugger;
pub use bidi_lite::rdp::resources::parse_resources;
pub use bidi_lite::tab::Tab;

pub use browser::Browser;
pub use config::{BrowserConfig, BrowserConfigBuilder, LaunchMode, PrefValue, ProfileMode};
pub use error::{BrowserError, ConfigError, EarlyExitReason, ProfileError};
pub use pool::{BrowserEntry, BrowserId, BrowserPool};
pub use ports::PortAllocator;

/// The README's code blocks, compiled as doctests (they use `test_support`).
#[cfg(all(doctest, feature = "test-support"))]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

/// Polls `check` every 20 ms for up to 5 s (unit-test helper).
#[cfg(test)]
pub(crate) async fn test_support_wait(check: impl Fn() -> bool) -> bool {
    for _ in 0..250 {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    check()
}
