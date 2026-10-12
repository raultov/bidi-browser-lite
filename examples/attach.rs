//! Attach to a Firefox that is already running, list its tabs, detach.
//!
//! Start one first with `tests/firefox.sh start` (BiDi on 127.0.0.1:9322).
//! `stop()` on an attached browser only ends this crate's BiDi session: the
//! Firefox keeps running.

use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let port = std::env::var("BIDI_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9322);
    let config = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .port(port)
        .build();
    let browser = Browser::ensure(config).await?;
    let tabs = browser.list_tabs().await;
    // Ends the session even when listing failed: Firefox allows only one.
    browser.stop().await?;
    let tabs = tabs?;
    for tab in &tabs {
        println!("  {} {}", tab.context, tab.url);
    }
    println!("Attached to Firefox, tabs: {}", tabs.len());
    Ok(())
}
