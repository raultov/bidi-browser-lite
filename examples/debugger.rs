//! Launch Firefox with its debugger server (RDP, opt-in), pause on an
//! uncaught exception, read a variable in the paused frame, resume.
//!
//! BiDi drives the page; RDP debugs it. BiDi context ids and RDP tab
//! descriptors are unrelated, so the tab is brought to the front and the
//! debugger attaches to the selected tab.

use std::time::Duration;

use bidi_browser_lite::bidi_lite::rdp::debugger::{
    PauseOnExceptions, ThreadState, parse_thread_states,
};
use bidi_browser_lite::{Browser, BrowserConfig, Debugger, LaunchMode, ReadinessState, Tab};
use futures_util::StreamExt;
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
        .rdp(true)
        .headless(headless)
        .build();
    let browser = Browser::ensure(config).await?;
    let result = run(&browser).await;
    browser.stop().await?;
    result
}

async fn run(browser: &Browser) -> Result<(), Box<dyn std::error::Error>> {
    let tab = browser.new_tab("about:blank").await?;
    tab.navigate("https://www.rust-lang.org", ReadinessState::Complete)
        .await?;
    tab.activate().await?;
    let debugger = Debugger::attach_selected(&browser.rdp_client().await?).await?;
    let result = pause_and_inspect(&tab, &debugger).await;
    // Detaching removes breakpoints and resumes: a paused page would freeze.
    debugger.detach().await?;
    result
}

async fn pause_and_inspect(
    tab: &Tab,
    debugger: &Debugger,
) -> Result<(), Box<dyn std::error::Error>> {
    debugger
        .set_pause_on_exceptions(PauseOnExceptions::Uncaught)
        .await?;
    let mut states = debugger.on_thread_state();
    let throw_later = json!({
        "expression": "setTimeout(() => { const answer = 42; throw new Error('boom ' + answer) })",
        "target": tab.script_target(),
        "awaitPromise": false,
    });
    tab.send_raw_command("script.evaluate", throw_later).await?;
    let paused = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(packet)) = states.next().await {
            for state in parse_thread_states(&packet) {
                if let ThreadState::Paused {
                    why,
                    frame: Some(frame),
                } = state
                {
                    return Some((why, frame));
                }
            }
        }
        None
    })
    .await?;
    let (why, frame) = paused.ok_or("the thread-state stream ended")?;
    let answer = debugger.evaluate_on_frame(&frame.actor, "answer").await?;
    println!("Paused on {}: answer = {}", why["type"], answer.result);
    debugger.resume().await?;
    debugger
        .set_pause_on_exceptions(PauseOnExceptions::None)
        .await?;
    Ok(())
}
