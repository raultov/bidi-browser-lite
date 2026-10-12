//! Launch a managed Firefox, open a tab, read the page title, stop.
//!
//! Visible window by default; `BIDI_BROWSER_LITE_HEADLESS=1` runs headless.

use bidi_browser_lite::{Browser, BrowserConfig, LaunchMode, ReadinessState};
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let headless = std::env::var_os("BIDI_BROWSER_LITE_HEADLESS").is_some();
    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .headless(headless)
        .build();
    let browser = Browser::ensure(config).await?;
    // Stop even if something below fails: a managed Firefox is ours to close.
    let result = run(&browser).await;
    browser.stop().await?;
    result
}

async fn run(browser: &Browser) -> Result<(), Box<dyn std::error::Error>> {
    let (host, port) = browser.debug_address().await;
    println!(
        "Firefox launched: BiDi on {host}:{port}, pid {:?}",
        browser.pid().await
    );
    let tab = browser.new_tab("about:blank").await?;
    tab.navigate("https://www.rust-lang.org", ReadinessState::Complete)
        .await?;
    let params = json!({"expression": "document.title", "target": tab.script_target(), "awaitPromise": false});
    let reply = tab.send_raw_command("script.evaluate", params).await?;
    let title = reply
        .result
        .as_ref()
        .and_then(|r| r["result"]["value"].as_str())
        .unwrap_or_default()
        .to_owned();
    println!("Title: {title}");
    Ok(())
}
